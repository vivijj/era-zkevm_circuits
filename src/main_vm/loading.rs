use boojum::{
    algebraic_props::round_function::AlgebraicRoundFunction,
    gadgets::{
        traits::round_function::CircuitRoundFunction,
        u160::UInt160,
        u256::{decompose_u256_as_u32x8, UInt256},
    },
};

use super::*;
use crate::base_structures::{
    register::VMRegister,
    vm_state::{
        callstack::{Callstack, FullExecutionContext},
        VmLocalState, FULL_SPONGE_QUEUE_STATE_WIDTH, QUEUE_STATE_WIDTH,
    },
};

pub fn initial_bootloader_state<
    F: SmallField,
    CS: ConstraintSystem<F>,
    R: CircuitRoundFunction<F, 8, 12, 4> + AlgebraicRoundFunction<F, 8, 12, 4>,
>(
    cs: &mut CS,
    memory_queue_initial_length: UInt32<F>,
    memory_queue_initial_tail: [Num<F>; FULL_SPONGE_QUEUE_STATE_WIDTH],
    decommitment_queue_initial_length: UInt32<F>,
    decommitment_queue_initial_tail: [Num<F>; FULL_SPONGE_QUEUE_STATE_WIDTH],
    initial_rollback_queue_value: [Num<F>; QUEUE_STATE_WIDTH],
    _round_function: &R,
) -> VmLocalState<F> {
    // first create the context
    let mut ctx = FullExecutionContext::uninitialized(cs);

    ctx.saved_context.base_page =
        UInt32::allocated_constant(cs, zkevm_opcode_defs::BOOTLOADER_BASE_PAGE);
    ctx.saved_context.code_page =
        UInt32::allocated_constant(cs, zkevm_opcode_defs::BOOTLOADER_CODE_PAGE);

    let zero_num = Num::zero(cs);
    let zero_u32 = UInt32::zero(cs);
    let zero_u16 = UInt16::zero(cs);
    let _boolean_false = Boolean::allocated_constant(cs, false);
    let boolean_true = Boolean::allocated_constant(cs, true);

    ctx.saved_context.pc = zero_u16;
    ctx.saved_context.exception_handler_loc = UInt16::allocated_constant(
        cs,
        zkevm_opcode_defs::system_params::INITIAL_FRAME_FORMAL_EH_LOCATION,
    );
    ctx.saved_context.ergs_remaining =
        UInt32::allocated_constant(cs, zkevm_opcode_defs::system_params::VM_INITIAL_FRAME_ERGS);

    let formal_bootloader_address_low = UInt32::allocated_constant(
        cs,
        zkevm_opcode_defs::system_params::BOOTLOADER_FORMAL_ADDRESS_LOW as u32,
    );

    let formal_bootloader_address =
        UInt160 { inner: [formal_bootloader_address_low, zero_u32, zero_u32, zero_u32, zero_u32] };

    ctx.saved_context.code_address = formal_bootloader_address;
    ctx.saved_context.this = formal_bootloader_address;
    ctx.saved_context.caller = UInt160::zero(cs); // is called from nowhere

    // circuit specific bit
    ctx.saved_context.reverted_queue_tail = initial_rollback_queue_value;
    ctx.saved_context.reverted_queue_head = ctx.saved_context.reverted_queue_tail;

    // mark as kernel
    ctx.saved_context.is_kernel_mode = boolean_true;

    // bootloader should not pay for resizes
    ctx.saved_context.heap_upper_bound =
        UInt32::allocated_constant(cs, zkevm_opcode_defs::system_params::BOOTLOADER_MAX_MEMORY);
    ctx.saved_context.aux_heap_upper_bound =
        UInt32::allocated_constant(cs, zkevm_opcode_defs::system_params::BOOTLOADER_MAX_MEMORY);

    // now push that to the callstack, manually

    let mut empty_entry = FullExecutionContext::uninitialized(cs);
    empty_entry.saved_context.reverted_queue_tail = initial_rollback_queue_value;
    empty_entry.saved_context.reverted_queue_head = ctx.saved_context.reverted_queue_tail;
    empty_entry.saved_context.is_kernel_mode = boolean_true;

    use boojum::gadgets::traits::encodable::CircuitEncodable;
    let empty_entry_encoding = empty_entry.saved_context.encode(cs); // only saved part

    let callstack_empty_state = [zero_num; FULL_SPONGE_QUEUE_STATE_WIDTH];

    let mut current_state = callstack_empty_state.map(|el| el.get_variable());
    assert!(empty_entry_encoding.len() % 8 == 0);

    for encoding_chunk in empty_entry_encoding.array_chunks::<8>() {
        // absorb by replacement
        let initial_state = [
            encoding_chunk[0],
            encoding_chunk[1],
            encoding_chunk[2],
            encoding_chunk[3],
            encoding_chunk[4],
            encoding_chunk[5],
            encoding_chunk[6],
            encoding_chunk[7],
            current_state[8],
            current_state[9],
            current_state[10],
            current_state[11],
        ];

        let final_state = R::compute_round_function(cs, initial_state);

        current_state = final_state;
    }

    let callstack_initial_state = current_state.map(|el| Num::from_variable(el));

    let callstack_depth = UInt32::allocated_constant(cs, 1u32);

    let callstack = Callstack {
        current_context: ctx,
        context_stack_depth: callstack_depth,
        stack_sponge_state: callstack_initial_state,
    };

    let mut bootloaded_state = VmLocalState::uninitialized(cs);
    // memory
    bootloaded_state.memory_queue_length = memory_queue_initial_length;
    bootloaded_state.memory_queue_state = memory_queue_initial_tail;
    // code decommittments
    bootloaded_state.code_decommittment_queue_length = decommitment_queue_initial_length;
    bootloaded_state.code_decommittment_queue_state = decommitment_queue_initial_tail;
    // rest
    bootloaded_state.callstack = callstack;
    // timestamp and global counters
    bootloaded_state.timestamp =
        UInt32::allocated_constant(cs, zkevm_opcode_defs::STARTING_TIMESTAMP);
    bootloaded_state.memory_page_counter =
        UInt32::allocated_constant(cs, zkevm_opcode_defs::STARTING_BASE_PAGE);

    // we also FORMALLY mark r1 as "pointer" type, even though we will NOT have any calldata
    // Nevertheless we put it "formally" to make an empty slice to designated page

    let formal_ptr = zkevm_opcode_defs::FatPointer {
        offset: 0,
        memory_page: zkevm_opcode_defs::BOOTLOADER_CALLDATA_PAGE,
        start: 0,
        length: 0,
    };
    let formal_ptr_encoding = formal_ptr.to_u256();

    let decomposition = decompose_u256_as_u32x8(formal_ptr_encoding);
    let l0 = UInt32::allocated_constant(cs, decomposition[0]);
    let l1 = UInt32::allocated_constant(cs, decomposition[1]);
    let l2 = UInt32::allocated_constant(cs, decomposition[2]);
    let l3 = UInt32::allocated_constant(cs, decomposition[3]);

    debug_assert_eq!(decomposition[4], 0);
    debug_assert_eq!(decomposition[5], 0);
    debug_assert_eq!(decomposition[6], 0);
    debug_assert_eq!(decomposition[7], 0);

    bootloaded_state.registers[0] = VMRegister {
        is_pointer: boolean_true,
        value: UInt256 { inner: [l0, l1, l2, l3, zero_u32, zero_u32, zero_u32, zero_u32] },
    };

    bootloaded_state
}
