use boojum::{
    cs::{traits::cs::ConstraintSystem, Variable},
    field::SmallField,
    gadgets::{
        boolean::Boolean,
        queue::*,
        traits::{
            allocatable::*, auxiliary::PrettyComparison, encodable::CircuitVarLengthEncodable,
            selectable::Selectable, witnessable::WitnessHookable,
        },
        u8::UInt8,
    },
    serde_utils::BigArraySerde,
};
use cs_derive::*;
use derivative::*;

use crate::base_structures::{
    log_query::{LogQuery, LOG_QUERY_PACKED_WIDTH},
    vm_state::*,
};

#[derive(Derivative, CSAllocatable, CSSelectable, CSVarLengthEncodable, WitnessHookable)]
#[derivative(Clone, Copy, Debug)]
#[DerivePrettyComparison("true")]
pub struct LinearHasherInputData<F: SmallField> {
    pub queue_state: QueueState<F, QUEUE_STATE_WIDTH>,
}

impl<F: SmallField> CSPlaceholder<F> for LinearHasherInputData<F> {
    fn placeholder<CS: ConstraintSystem<F>>(cs: &mut CS) -> Self {
        Self { queue_state: QueueState::<F, QUEUE_STATE_WIDTH>::placeholder(cs) }
    }
}

#[derive(Derivative, CSAllocatable, CSSelectable, CSVarLengthEncodable, WitnessHookable)]
#[derivative(Clone, Copy, Debug)]
#[DerivePrettyComparison("true")]
pub struct LinearHasherOutputData<F: SmallField> {
    pub keccak256_hash: [UInt8<F>; 32],
}

impl<F: SmallField> CSPlaceholder<F> for LinearHasherOutputData<F> {
    fn placeholder<CS: ConstraintSystem<F>>(cs: &mut CS) -> Self {
        Self { keccak256_hash: [UInt8::<F>::placeholder(cs); 32] }
    }
}

pub type LinearHasherInputOutput<F> = crate::fsm_input_output::ClosedFormInput<
    F,
    (),
    LinearHasherInputData<F>,
    LinearHasherOutputData<F>,
>;

pub type LinearHasherInputOutputWitness<F> = crate::fsm_input_output::ClosedFormInputWitness<
    F,
    (),
    LinearHasherInputData<F>,
    LinearHasherOutputData<F>,
>;

#[derive(Derivative, serde::Serialize, serde::Deserialize)]
#[derivative(Clone, Debug, Default)]
#[serde(bound = "")]
pub struct LinearHasherCircuitInstanceWitness<F: SmallField> {
    pub closed_form_input: LinearHasherInputOutputWitness<F>,
    // #[serde(bound(
    //     serialize = "CircuitQueueRawWitness<F, LogQuery<F>, 4, LOG_QUERY_PACKED_WIDTH>:
    // serde::Serialize" ))]
    // #[serde(bound(
    //     deserialize = "CircuitQueueRawWitness<F, LogQuery<F>, 4, LOG_QUERY_PACKED_WIDTH>:
    // serde::de::DeserializeOwned" ))]
    pub queue_witness: CircuitQueueRawWitness<F, LogQuery<F>, 4, LOG_QUERY_PACKED_WIDTH>,
}
