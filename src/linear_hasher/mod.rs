use std::{mem::MaybeUninit, sync::Arc};

use boojum::{
    algebraic_props::round_function::AlgebraicRoundFunction,
    cs::traits::cs::ConstraintSystem,
    field::SmallField,
    gadgets::{
        boolean::Boolean,
        keccak256,
        num::Num,
        queue::CircuitQueueWitness,
        traits::{
            allocatable::{CSAllocatableExt, CSPlaceholder},
            round_function::CircuitRoundFunction,
            selectable::Selectable,
        },
        u256::UInt256,
        u8::UInt8,
    },
};

use super::*;
use crate::{
    base_structures::log_query::LogQuery, demux_log_queue::StorageLogQueue,
    fsm_input_output::circuit_inputs::INPUT_OUTPUT_COMMITMENT_LENGTH,
};

pub mod input;
use self::input::*;

pub fn linear_hasher_entry_point<
    F: SmallField,
    CS: ConstraintSystem<F>,
    R: CircuitRoundFunction<F, 8, 12, 4> + AlgebraicRoundFunction<F, 8, 12, 4>,
>(
    cs: &mut CS,
    witness: LinearHasherCircuitInstanceWitness<F>,
    round_function: &R,
    params: usize,
) -> [Num<F>; INPUT_OUTPUT_COMMITMENT_LENGTH]
where
    [(); <LogQuery<F> as CSAllocatableExt<F>>::INTERNAL_STRUCT_LEN]:,
    [(); <UInt256<F> as CSAllocatableExt<F>>::INTERNAL_STRUCT_LEN]:,
    [(); <UInt256<F> as CSAllocatableExt<F>>::INTERNAL_STRUCT_LEN + 1]:,
{
    let limit = params;

    assert!(limit <= u32::MAX as usize);

    let LinearHasherCircuitInstanceWitness { closed_form_input, queue_witness } = witness;

    let mut structured_input =
        LinearHasherInputOutput::alloc_ignoring_outputs(cs, closed_form_input.clone());
    let start_flag = structured_input.start_flag;

    let zero_u8: UInt8<F> = UInt8::zero(cs);
    let boolean_true = Boolean::allocated_constant(cs, true);

    // only 1 instance of the circuit here for now
    Boolean::enforce_equal(cs, &start_flag, &boolean_true);

    let queue_state_from_input = structured_input.observable_input.queue_state;

    // it must be trivial
    queue_state_from_input.enforce_trivial_head(cs);

    let mut queue = StorageLogQueue::<F, R>::from_state(cs, queue_state_from_input);
    let queue_witness = CircuitQueueWitness::from_inner_witness(queue_witness);
    queue.witness = Arc::new(queue_witness);

    let keccak_accumulator_state =
        [[[zero_u8; keccak256::BYTES_PER_WORD]; keccak256::LANE_WIDTH]; keccak256::LANE_WIDTH];

    let mut keccak_accumulator_state =
        keccak_accumulator_state.map(|el| el.map(|el| el.map(|el| el.get_variable())));

    // we do not serialize length because it's recalculatable in L1

    let empty_hash = {
        use zkevm_opcode_defs::sha3::*;

        let mut result = [0u8; 32];
        let digest = Keccak256::digest(&[]);
        result.copy_from_slice(digest.as_slice());

        result.map(|el| UInt8::allocated_constant(cs, el))
    };

    let mut buffer = vec![];

    let mut done = queue.is_empty(cs);
    let no_work = done;

    use boojum::gadgets::keccak256::KECCAK_RATE_BYTES;

    use crate::storage_application::keccak256_conditionally_absorb_and_run_permutation;

    for _cycle in 0..limit {
        let queue_is_empty = queue.is_empty(cs);
        let should_pop = queue_is_empty.negated(cs);

        let (storage_log, _) = queue.pop_front(cs, should_pop);

        let now_empty = queue.is_empty(cs);
        let is_last_serialization = Boolean::multi_and(cs, &[should_pop, now_empty]);
        use crate::base_structures::ByteSerializable;
        let as_bytes = storage_log.into_bytes(cs);

        assert!(buffer.len() < 136);

        buffer.extend(as_bytes);

        let continue_to_absorb = done.negated(cs);

        if buffer.len() >= 136 {
            let buffer_for_round: [UInt8<F>; KECCAK_RATE_BYTES] = buffer[..136].try_into().unwrap();
            let buffer_for_round = buffer_for_round.map(|el| el.get_variable());
            let carry_on = buffer[136..].to_vec();

            buffer = carry_on;

            // absorb if we are not done yet
            keccak256_conditionally_absorb_and_run_permutation(
                cs,
                continue_to_absorb,
                &mut keccak_accumulator_state,
                &buffer_for_round,
            );
        }

        assert!(buffer.len() < 136);

        // in case if we do last round
        {
            let absorb_as_last_round =
                Boolean::multi_and(cs, &[continue_to_absorb, is_last_serialization]);
            let mut last_round_buffer = [zero_u8; KECCAK_RATE_BYTES];
            let tail_len = buffer.len();
            last_round_buffer[..tail_len].copy_from_slice(&buffer);

            if tail_len == KECCAK_RATE_BYTES - 1 {
                // unreachable, but we set it for completeness
                last_round_buffer[tail_len] = UInt8::allocated_constant(cs, 0x81);
            } else {
                last_round_buffer[tail_len] = UInt8::allocated_constant(cs, 0x01);
                last_round_buffer[KECCAK_RATE_BYTES - 1] = UInt8::allocated_constant(cs, 0x80);
            }

            let last_round_buffer = last_round_buffer.map(|el| el.get_variable());

            // absorb if it's the last round
            keccak256_conditionally_absorb_and_run_permutation(
                cs,
                absorb_as_last_round,
                &mut keccak_accumulator_state,
                &last_round_buffer,
            );
        }

        done = Boolean::multi_or(cs, &[done, is_last_serialization]);
    }

    queue.enforce_consistency(cs);
    let completed = queue.is_empty(cs);

    Boolean::enforce_equal(cs, &completed, &boolean_true);

    structured_input.completion_flag = completed.clone();

    let fsm_output = ();
    structured_input.hidden_fsm_output = fsm_output;

    // squeeze
    let mut keccak256_hash = [MaybeUninit::<UInt8<F>>::uninit(); keccak256::KECCAK256_DIGEST_SIZE];
    for (i, dst) in keccak256_hash.array_chunks_mut::<8>().enumerate() {
        for (dst, src) in dst.iter_mut().zip(keccak_accumulator_state[i][0].iter()) {
            let tmp = unsafe { UInt8::from_variable_unchecked(*src) };
            dst.write(tmp);
        }
    }

    let keccak256_hash = unsafe { keccak256_hash.map(|el| el.assume_init()) };

    let keccak256_hash =
        <[UInt8<F>; 32]>::conditionally_select(cs, no_work, &empty_hash, &keccak256_hash);

    let mut observable_output = LinearHasherOutputData::placeholder(cs);
    observable_output.keccak256_hash = keccak256_hash;
    structured_input.observable_output = observable_output;

    // self-check
    structured_input.hook_compare_witness(cs, &closed_form_input);

    use boojum::cs::gates::PublicInputGate;

    use crate::fsm_input_output::{
        commit_variable_length_encodable_item, ClosedFormInputCompactForm,
    };

    let compact_form =
        ClosedFormInputCompactForm::from_full_form(cs, &structured_input, round_function);
    let input_commitment = commit_variable_length_encodable_item(cs, &compact_form, round_function);
    for el in input_commitment.iter() {
        let gate = PublicInputGate::new(el.get_variable());
        gate.add_to_cs(cs);
    }

    input_commitment
}
