use std::collections::VecDeque;

use boojum::{
    cs::Variable,
    gadgets::{
        queue::*,
        traits::{
            allocatable::{CSAllocatable, CSPlaceholder},
            auxiliary::PrettyComparison,
            encodable::CircuitVarLengthEncodable,
        },
    },
};

use super::*;
use crate::base_structures::{precompile_input_outputs::*, vm_state::*};

#[derive(Derivative, CSAllocatable, CSSelectable, CSVarLengthEncodable, WitnessHookable)]
#[derivative(Clone, Copy, Debug)]
#[DerivePrettyComparison("true")]
pub struct Secp256r1VerifyCircuitFSMInputOutput<F: SmallField> {
    pub log_queue_state: QueueState<F, QUEUE_STATE_WIDTH>,
    pub memory_queue_state: QueueState<F, FULL_SPONGE_QUEUE_STATE_WIDTH>,
}

impl<F: SmallField> CSPlaceholder<F> for Secp256r1VerifyCircuitFSMInputOutput<F> {
    fn placeholder<CS: ConstraintSystem<F>>(cs: &mut CS) -> Self {
        Self {
            log_queue_state: QueueState::<F, QUEUE_STATE_WIDTH>::placeholder(cs),
            memory_queue_state: QueueState::<F, FULL_SPONGE_QUEUE_STATE_WIDTH>::placeholder(cs),
        }
    }
}

pub type Secp256r1VerifyCircuitInputOutput<F> = ClosedFormInput<
    F,
    Secp256r1VerifyCircuitFSMInputOutput<F>,
    PrecompileFunctionInputData<F>,
    PrecompileFunctionOutputData<F>,
>;
pub type Secp256r1VerifyCircuitInputOutputWitness<F> = ClosedFormInputWitness<
    F,
    Secp256r1VerifyCircuitFSMInputOutput<F>,
    PrecompileFunctionInputData<F>,
    PrecompileFunctionOutputData<F>,
>;

#[derive(Derivative, serde::Serialize, serde::Deserialize)]
#[derivative(Clone, Debug, Default)]
#[serde(bound = "")]
pub struct Secp256r1VerifyCircuitInstanceWitness<F: SmallField> {
    pub closed_form_input: Secp256r1VerifyCircuitInputOutputWitness<F>,
    pub requests_queue_witness: CircuitQueueRawWitness<F, LogQuery<F>, 4, LOG_QUERY_PACKED_WIDTH>,
    pub memory_reads_witness: VecDeque<[U256; MEMORY_QUERIES_PER_CALL]>,
}
