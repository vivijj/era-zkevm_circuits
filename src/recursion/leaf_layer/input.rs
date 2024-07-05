use boojum::{
    cs::{
        implementations::{proof::Proof, verifier::VerificationKey},
        traits::cs::ConstraintSystem,
        Variable,
    },
    field::{FieldExtension, SmallField},
    gadgets::{
        boolean::Boolean,
        num::Num,
        queue::full_state_queue::FullStateCircuitQueueRawWitness,
        traits::{
            allocatable::*, auxiliary::PrettyComparison, encodable::CircuitVarLengthEncodable,
            selectable::Selectable, witnessable::WitnessHookable,
        },
    },
    serde_utils::BigArraySerde,
};
use cs_derive::*;

use super::*;
use crate::base_structures::{recursion_query::*, vm_state::*};

#[derive(Derivative, CSAllocatable, CSSelectable, CSVarLengthEncodable, WitnessHookable)]
#[derivative(Clone, Copy, Debug)]
#[DerivePrettyComparison("true")]
pub struct RecursionLeafParameters<F: SmallField> {
    pub circuit_type: Num<F>,
    pub basic_circuit_vk_commitment: [Num<F>; VK_COMMITMENT_LENGTH],
    pub leaf_layer_vk_commitment: [Num<F>; VK_COMMITMENT_LENGTH],
}

impl<F: SmallField> CSPlaceholder<F> for RecursionLeafParameters<F> {
    fn placeholder<CS: ConstraintSystem<F>>(cs: &mut CS) -> Self {
        let zero = Num::zero(cs);
        Self {
            circuit_type: zero,
            basic_circuit_vk_commitment: [zero; VK_COMMITMENT_LENGTH],
            leaf_layer_vk_commitment: [zero; VK_COMMITMENT_LENGTH],
        }
    }
}

impl<F: SmallField> RecursionLeafParameters<F> {
    pub fn allocated_constant<CS: ConstraintSystem<F>>(
        cs: &mut CS,
        value: <Self as CSAllocatable<F>>::Witness,
    ) -> Self {
        let circuit_type = Num::allocated_constant(cs, value.circuit_type);
        let basic_circuit_vk_commitment = value
            .basic_circuit_vk_commitment
            .map(|el| Num::allocated_constant(cs, el));
        let leaf_layer_vk_commitment = value
            .leaf_layer_vk_commitment
            .map(|el| Num::allocated_constant(cs, el));

        Self { circuit_type, basic_circuit_vk_commitment, leaf_layer_vk_commitment }
    }
}

#[derive(Derivative, CSAllocatable, CSSelectable, CSVarLengthEncodable, WitnessHookable)]
#[derivative(Clone, Copy, Debug)]
#[DerivePrettyComparison("true")]
pub struct RecursionLeafInput<F: SmallField> {
    pub params: RecursionLeafParameters<F>,
    pub queue_state: QueueState<F, FULL_SPONGE_QUEUE_STATE_WIDTH>,
}

impl<F: SmallField> CSPlaceholder<F> for RecursionLeafInput<F> {
    fn placeholder<CS: ConstraintSystem<F>>(cs: &mut CS) -> Self {
        Self {
            params: RecursionLeafParameters::placeholder(cs),
            queue_state: QueueState::<F, FULL_SPONGE_QUEUE_STATE_WIDTH>::placeholder(cs),
        }
    }
}

#[derive(Derivative, serde::Serialize, serde::Deserialize)]
#[derivative(Clone, Debug(bound = ""), Default(bound = "RecursionLeafInputWitness<F>: Default"))]
#[serde(
    bound = "<H::CircuitOutput as CSAllocatable<F>>::Witness: serde::Serialize + serde::de::DeserializeOwned"
)]
pub struct RecursionLeafInstanceWitness<
    F: SmallField,
    H: RecursiveTreeHasher<F, Num<F>>,
    EXT: FieldExtension<2, BaseField = F>,
> {
    pub input: RecursionLeafInputWitness<F>,
    pub vk_witness: VerificationKey<F, H::NonCircuitSimulator>,
    pub queue_witness: FullStateCircuitQueueRawWitness<
        F,
        RecursionQuery<F>,
        FULL_SPONGE_QUEUE_STATE_WIDTH,
        RECURSION_QUERY_PACKED_WIDTH,
    >,
    pub proof_witnesses: VecDeque<Proof<F, H::NonCircuitSimulator, EXT>>,
}
