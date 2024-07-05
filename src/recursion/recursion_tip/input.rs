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
        traits::{
            allocatable::*, auxiliary::PrettyComparison, encodable::CircuitVarLengthEncodable,
            selectable::Selectable, witnessable::WitnessHookable,
        },
    },
    serde_utils::BigArraySerde,
};
use cs_derive::*;

use super::*;
use crate::{base_structures::vm_state::*, recursion::leaf_layer::input::RecursionLeafParameters};

pub const RECURSION_TIP_ARITY: usize = 32;

#[derive(Derivative, CSAllocatable, CSSelectable, CSVarLengthEncodable, WitnessHookable)]
#[derivative(Clone, Copy, Debug)]
#[DerivePrettyComparison("true")]
pub struct RecursionTipInput<F: SmallField> {
    pub leaf_layer_parameters: [RecursionLeafParameters<F>; NUM_BASE_LAYER_CIRCUITS],
    pub node_layer_vk_commitment: [Num<F>; VK_COMMITMENT_LENGTH],
    pub branch_circuit_type_set: [Num<F>; RECURSION_TIP_ARITY],
    pub queue_set: [QueueState<F, FULL_SPONGE_QUEUE_STATE_WIDTH>; RECURSION_TIP_ARITY],
}

impl<F: SmallField> CSPlaceholder<F> for RecursionTipInput<F> {
    fn placeholder<CS: ConstraintSystem<F>>(cs: &mut CS) -> Self {
        let zero = Num::zero(cs);
        let leaf_layer_param = RecursionLeafParameters::placeholder(cs);
        Self {
            leaf_layer_parameters: [leaf_layer_param; NUM_BASE_LAYER_CIRCUITS],
            node_layer_vk_commitment: [zero; VK_COMMITMENT_LENGTH],
            branch_circuit_type_set: [zero; RECURSION_TIP_ARITY],
            queue_set: [QueueState::<F, FULL_SPONGE_QUEUE_STATE_WIDTH>::placeholder(cs);
                RECURSION_TIP_ARITY],
        }
    }
}

#[derive(Derivative, serde::Serialize, serde::Deserialize)]
#[derivative(Clone, Debug, Default(bound = "RecursionTipInputWitness<F>: Default"))]
#[serde(
    bound = "<H::CircuitOutput as CSAllocatable<F>>::Witness: serde::Serialize + serde::de::DeserializeOwned"
)]
pub struct RecursionTipInstanceWitness<
    F: SmallField,
    H: RecursiveTreeHasher<F, Num<F>>,
    EXT: FieldExtension<2, BaseField = F>,
> {
    pub input: RecursionTipInputWitness<F>,
    pub vk_witness: VerificationKey<F, H::NonCircuitSimulator>,
    pub proof_witnesses: VecDeque<Proof<F, H::NonCircuitSimulator, EXT>>,
}
