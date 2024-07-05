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

#[derive(Derivative, CSAllocatable, CSSelectable, CSVarLengthEncodable, WitnessHookable)]
#[derivative(Clone, Copy, Debug)]
#[DerivePrettyComparison("true")]
pub struct RecursionNodeInput<F: SmallField> {
    pub branch_circuit_type: Num<F>,
    pub leaf_layer_parameters: [RecursionLeafParameters<F>; NUM_BASE_LAYER_CIRCUITS],
    pub node_layer_vk_commitment: [Num<F>; VK_COMMITMENT_LENGTH],
    pub queue_state: QueueState<F, FULL_SPONGE_QUEUE_STATE_WIDTH>,
}

impl<F: SmallField> CSPlaceholder<F> for RecursionNodeInput<F> {
    fn placeholder<CS: ConstraintSystem<F>>(cs: &mut CS) -> Self {
        let zero = Num::zero(cs);
        let leaf_layer_param = RecursionLeafParameters::placeholder(cs);
        Self {
            branch_circuit_type: zero,
            leaf_layer_parameters: [leaf_layer_param; NUM_BASE_LAYER_CIRCUITS],
            node_layer_vk_commitment: [zero; VK_COMMITMENT_LENGTH],
            queue_state: QueueState::<F, FULL_SPONGE_QUEUE_STATE_WIDTH>::placeholder(cs),
        }
    }
}

#[derive(Derivative, serde::Serialize, serde::Deserialize)]
#[derivative(Clone, Debug, Default(bound = "RecursionNodeInputWitness<F>: Default"))]
#[serde(
    bound = "<H::CircuitOutput as CSAllocatable<F>>::Witness: serde::Serialize + serde::de::DeserializeOwned"
)]
pub struct RecursionNodeInstanceWitness<
    F: SmallField,
    H: RecursiveTreeHasher<F, Num<F>>,
    EXT: FieldExtension<2, BaseField = F>,
> {
    pub input: RecursionNodeInputWitness<F>,
    pub vk_witness: VerificationKey<F, H::NonCircuitSimulator>,
    pub split_points: VecDeque<QueueTailStateWitness<F, FULL_SPONGE_QUEUE_STATE_WIDTH>>,
    pub proof_witnesses: VecDeque<Proof<F, H::NonCircuitSimulator, EXT>>,
}
