use super::*;

pub mod input;
use boojum::{
    cs::{
        implementations::{prover::ProofConfig, verifier::VerificationKey},
        oracle::TreeHasher,
        traits::{circuit::ErasedBuilderForRecursiveVerifier, cs::ConstraintSystem},
    },
    field::{FieldExtension, SmallField},
    gadgets::{
        boolean::Boolean,
        num::Num,
        recursion::{
            allocated_proof::AllocatedProof, allocated_vk::AllocatedVerificationKey,
            circuit_pow::RecursivePoWRunner, recursive_transcript::*, recursive_tree_hasher::*,
        },
        traits::allocatable::CSAllocatable,
    },
};

pub use self::input::*;
use crate::fsm_input_output::circuit_inputs::INPUT_OUTPUT_COMMITMENT_LENGTH;

// We recursively verify SINGLE proofs over FIXED VK and output it's inputs

#[derive(Derivative, serde::Serialize, serde::Deserialize)]
#[derivative(Clone, Debug)]
#[serde(bound = "H::Output: serde::Serialize + serde::de::DeserializeOwned")]
pub struct CompressionRecursionConfig<
    F: SmallField,
    H: TreeHasher<F>,
    EXT: FieldExtension<2, BaseField = F>,
> {
    pub proof_config: ProofConfig,
    pub verification_key: VerificationKey<F, H>,
    pub _marker: std::marker::PhantomData<(F, H, EXT)>,
}

pub fn proof_compression_function<
    F: SmallField,
    CS: ConstraintSystem<F> + 'static,
    H: RecursiveTreeHasher<F, Num<F>>,
    EXT: FieldExtension<2, BaseField = F>,
    TR: RecursiveTranscript<
            F,
            CompatibleCap = <H::NonCircuitSimulator as TreeHasher<F>>::Output,
            CircuitReflection = CTR,
        >,
    CTR: CircuitTranscript<
            F,
            CircuitCompatibleCap = <H as CircuitTreeHasher<F, Num<F>>>::CircuitOutput,
            TransciptParameters = TR::TransciptParameters,
        >,
    POW: RecursivePoWRunner<F>,
>(
    cs: &mut CS,
    witness: CompressionCircuitInstanceWitness<F, H, EXT>,
    config: CompressionRecursionConfig<F, H::NonCircuitSimulator, EXT>,
    verifier_builder: Box<dyn ErasedBuilderForRecursiveVerifier<F, EXT, CS>>,
    transcript_params: TR::TransciptParameters,
) {
    let CompressionCircuitInstanceWitness { proof_witness } = witness;

    // as usual - create verifier for FIXED VK, verify, aggregate inputs, output inputs

    let CompressionRecursionConfig { proof_config, verification_key, .. } = config;

    // use this and deal with borrow checker

    let r = cs as *mut CS;

    assert_eq!(verification_key.fixed_parameters.parameters, verifier_builder.geometry());

    let fixed_parameters = verification_key.fixed_parameters.clone();

    let verifier = verifier_builder.create_recursive_verifier(cs);

    let cs = unsafe { &mut *r };

    let vk = AllocatedVerificationKey::allocate_constant(cs, verification_key);

    let proof = AllocatedProof::allocate_from_witness(
        cs,
        proof_witness,
        &verifier,
        &fixed_parameters,
        &proof_config,
    );

    // verify the proof
    let (is_valid, public_inputs) = verifier.verify::<H, TR, CTR, POW>(
        cs,
        transcript_params.clone(),
        &proof,
        &fixed_parameters,
        &proof_config,
        &vk,
    );

    let boolean_true = Boolean::allocated_constant(cs, true);
    Boolean::enforce_equal(cs, &is_valid, &boolean_true);

    assert_eq!(public_inputs.len(), INPUT_OUTPUT_COMMITMENT_LENGTH);
    assert_eq!(public_inputs.len(), fixed_parameters.num_public_inputs());

    for el in public_inputs.into_iter() {
        use boojum::cs::gates::PublicInputGate;
        let gate = PublicInputGate::new(el.get_variable());
        gate.add_to_cs(cs);
    }
}
