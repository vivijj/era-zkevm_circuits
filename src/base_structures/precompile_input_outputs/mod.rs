use boojum::{
    cs::{traits::cs::ConstraintSystem, Variable},
    field::SmallField,
    gadgets::{
        boolean::Boolean,
        queue::QueueState,
        traits::{
            allocatable::{CSAllocatable, CSPlaceholder},
            auxiliary::PrettyComparison,
            encodable::CircuitVarLengthEncodable,
            selectable::Selectable,
            witnessable::WitnessHookable,
        },
    },
};
use cs_derive::*;

use super::*;
// universal precompiles passthrough input/output
// takes requests queue + memory state
// outputs memory state
use crate::base_structures::vm_state::*;

#[derive(Derivative, CSAllocatable, CSSelectable, CSVarLengthEncodable, WitnessHookable)]
#[derivative(Clone, Copy, Debug)]
#[DerivePrettyComparison("true")]
pub struct PrecompileFunctionInputData<F: SmallField> {
    pub initial_log_queue_state: QueueState<F, QUEUE_STATE_WIDTH>,
    pub initial_memory_queue_state: QueueState<F, FULL_SPONGE_QUEUE_STATE_WIDTH>,
}

impl<F: SmallField> CSPlaceholder<F> for PrecompileFunctionInputData<F> {
    fn placeholder<CS: ConstraintSystem<F>>(cs: &mut CS) -> Self {
        Self {
            initial_log_queue_state: QueueState::<F, QUEUE_STATE_WIDTH>::placeholder(cs),
            initial_memory_queue_state: QueueState::<F, FULL_SPONGE_QUEUE_STATE_WIDTH>::placeholder(
                cs,
            ),
        }
    }
}

#[derive(Derivative, CSAllocatable, CSSelectable, CSVarLengthEncodable, WitnessHookable)]
#[derivative(Clone, Copy, Debug)]
#[DerivePrettyComparison("true")]
pub struct PrecompileFunctionOutputData<F: SmallField> {
    pub final_memory_state: QueueState<F, FULL_SPONGE_QUEUE_STATE_WIDTH>,
}

impl<F: SmallField> CSPlaceholder<F> for PrecompileFunctionOutputData<F> {
    fn placeholder<CS: ConstraintSystem<F>>(cs: &mut CS) -> Self {
        Self { final_memory_state: QueueState::<F, FULL_SPONGE_QUEUE_STATE_WIDTH>::placeholder(cs) }
    }
}
