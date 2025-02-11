use std::{
    collections::VecDeque,
    sync::{Arc, RwLock},
};

use arrayvec::ArrayVec;
use boojum::{
    algebraic_props::round_function::AlgebraicRoundFunction,
    crypto_bigint::{Zero, U1024},
    cs::{gates::ConstantAllocatableCS, traits::cs::ConstraintSystem, Variable},
    field::SmallField,
    gadgets::{
        boolean::Boolean,
        curves::sw_projective::SWProjectivePoint,
        keccak256::keccak256,
        non_native_field::traits::NonNativeField,
        num::Num,
        queue::{CircuitQueueWitness, QueueState},
        tables::ByteSplitTable,
        traits::{
            allocatable::{CSAllocatableExt, CSPlaceholder},
            round_function::CircuitRoundFunction,
            selectable::Selectable,
        },
        u16::UInt16,
        u160::UInt160,
        u256::UInt256,
        u32::UInt32,
        u512::UInt512,
        u8::UInt8,
    },
    pairing::{ff::PrimeField, GenericCurveAffine},
};
use zkevm_opcode_defs::system_params::PRECOMPILE_AUX_BYTE;

pub use self::input::*;
use super::*;
use crate::{
    base_structures::precompile_input_outputs::PrecompileFunctionOutputData,
    demux_log_queue::StorageLogQueue,
    ecrecover::secp256k1::fixed_base_mul_table::FixedBaseMulTable, ethereum_types::U256,
    fsm_input_output::circuit_inputs::INPUT_OUTPUT_COMMITMENT_LENGTH,
};

pub const MEMORY_QUERIES_PER_CALL: usize = 4;
pub const ALLOW_ZERO_MESSAGE: bool = true;

#[derive(Derivative, CSSelectable)]
#[derivative(Clone, Debug)]
pub struct EcrecoverPrecompileCallParams<F: SmallField> {
    pub input_page: UInt32<F>,
    pub input_offset: UInt32<F>,
    pub output_page: UInt32<F>,
    pub output_offset: UInt32<F>,
}

impl<F: SmallField> EcrecoverPrecompileCallParams<F> {
    pub fn from_encoding<CS: ConstraintSystem<F>>(_cs: &mut CS, encoding: UInt256<F>) -> Self {
        let input_offset = encoding.inner[0];
        let output_offset = encoding.inner[2];
        let input_page = encoding.inner[4];
        let output_page = encoding.inner[5];

        let new = Self { input_page, input_offset, output_page, output_offset };

        new
    }
}

const NUM_WORDS: usize = 17;
const SECP_B_COEF: u64 = 7;
const EXCEPTION_FLAGS_ARR_LEN: usize = 9;
const NUM_MEMORY_READS_PER_CYCLE: usize = 4;
const X_POWERS_ARR_LEN: usize = 256;
const VALID_Y_IN_EXTERNAL_FIELD: u64 = 4;
const VALID_X_CUBED_IN_EXTERNAL_FIELD: u64 = 9;

// GLV consts

// Decomposition scalars can be a little more than 2^128 in practice, so we use 33 chunks of width 4
// bits
const MAX_DECOMPOSITION_VALUE: U256 = U256([u64::MAX, u64::MAX, 0x0f, 0]);
// BETA s.t. for any curve point Q = (x,y):
// lambda * Q = (beta*x mod p, y)
const BETA: &'static str =
    "55594575648329892869085402983802832744385952214688224221778511981742606582254";
// Secp256k1.p - 1 / 2
// 0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffc2f - 0x1 / 0x2
const MODULUS_MINUS_ONE_DIV_TWO: &'static str =
    "7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0";
// Decomposition constants
// Derived through algorithm 3.74 http://tomlr.free.fr/Math%E9matiques/Math%20Complete/Cryptography/Guide%20to%20Elliptic%20Curve%20Cryptography%20-%20D.%20Hankerson,%20A.%20Menezes,%20S.%20Vanstone.pdf
// NOTE: B2 == A1
const A1: &'static str = "0x3086d221a7d46bcde86c90e49284eb15";
const B1: &'static str = "0xe4437ed6010e88286f547fa90abfe4c3";
const A2: &'static str = "0x114ca50f7a8e2f3f657c1108d9d44cfd8";

const WINDOW_WIDTH: usize = 4;
const NUM_MULTIPLICATION_STEPS_FOR_WIDTH_4: usize = 33;
const PRECOMPUTATION_TABLE_SIZE: usize = (1 << WINDOW_WIDTH) - 1;

// assume that constructed field element is not zero
// if this is not satisfied - set the result to be F::one
fn convert_uint256_to_field_element_masked<
    F: SmallField,
    CS: ConstraintSystem<F>,
    P: boojum::pairing::ff::PrimeField,
    const N: usize,
>(
    cs: &mut CS,
    elem: &UInt256<F>,
    params: &Arc<NonNativeFieldOverU16Params<P, N>>,
) -> (NonNativeFieldOverU16<F, P, N>, Boolean<F>)
where
    [(); N + 1]:,
{
    let is_zero = elem.is_zero(cs);
    let one_nn = NonNativeFieldOverU16::<F, P, N>::allocated_constant(cs, P::one(), params);
    // we still have to decompose it into u16 words
    let zero_var = cs.allocate_constant(F::ZERO);
    let mut limbs = [zero_var; N];
    assert!(N >= 16);
    for (dst, src) in limbs.array_chunks_mut::<2>().zip(elem.inner.iter()) {
        let [b0, b1, b2, b3] = src.to_le_bytes(cs);
        let low = UInt16::from_le_bytes(cs, [b0, b1]);
        let high = UInt16::from_le_bytes(cs, [b2, b3]);

        *dst = [low.get_variable(), high.get_variable()];
    }

    let mut max_value = U1024::from_word(1u64);
    max_value = max_value.shl_vartime(256);
    max_value = max_value.saturating_sub(&U1024::from_word(1u64));

    let (overflows, rem) = max_value.div_rem(&params.modulus_u1024);

    assert!(overflows.lt(&U1024::from_word(1u64 << 32)));
    let mut max_moduluses = overflows.as_words()[0] as u32;
    if rem.is_zero().unwrap_u8() != 1 {
        max_moduluses += 1;
    }

    let element = NonNativeFieldOverU16 {
        limbs: limbs,
        non_zero_limbs: 16,
        tracker: OverflowTracker { max_moduluses },
        form: RepresentationForm::Normalized,
        params: params.clone(),
        _marker: std::marker::PhantomData,
    };

    let selected = Selectable::conditionally_select(cs, is_zero, &one_nn, &element);

    (selected, is_zero)
}

fn convert_uint256_to_field_element<
    F: SmallField,
    CS: ConstraintSystem<F>,
    P: boojum::pairing::ff::PrimeField,
    const N: usize,
>(
    cs: &mut CS,
    elem: &UInt256<F>,
    params: &Arc<NonNativeFieldOverU16Params<P, N>>,
) -> NonNativeFieldOverU16<F, P, N> {
    // we still have to decompose it into u16 words
    let zero_var = cs.allocate_constant(F::ZERO);
    let mut limbs = [zero_var; N];
    assert!(N >= 16);
    for (dst, src) in limbs.array_chunks_mut::<2>().zip(elem.inner.iter()) {
        let [b0, b1, b2, b3] = src.to_le_bytes(cs);
        let low = UInt16::from_le_bytes(cs, [b0, b1]);
        let high = UInt16::from_le_bytes(cs, [b2, b3]);

        *dst = [low.get_variable(), high.get_variable()];
    }

    let mut max_value = U1024::from_word(1u64);
    max_value = max_value.shl_vartime(256);
    max_value = max_value.saturating_sub(&U1024::from_word(1u64));

    let (overflows, rem) = max_value.div_rem(&params.modulus_u1024);
    assert!(overflows.lt(&U1024::from_word(1u64 << 32)));
    let mut max_moduluses = overflows.as_words()[0] as u32;
    if rem.is_zero().unwrap_u8() != 1 {
        max_moduluses += 1;
    }

    let element = NonNativeFieldOverU16 {
        limbs: limbs,
        non_zero_limbs: 16,
        tracker: OverflowTracker { max_moduluses },
        form: RepresentationForm::Normalized,
        params: params.clone(),
        _marker: std::marker::PhantomData,
    };

    element
}

// NOTE: caller must ensure that the field element is normalized, otherwise this will fail.
fn convert_field_element_to_uint256<
    F: SmallField,
    CS: ConstraintSystem<F>,
    P: boojum::pairing::ff::PrimeField,
    const N: usize,
>(
    cs: &mut CS,
    mut elem: NonNativeFieldOverU16<F, P, N>,
) -> UInt256<F> {
    assert_eq!(elem.form, RepresentationForm::Normalized);
    assert_eq!(elem.tracker.max_moduluses, 1);

    let mut limbs = [UInt32::<F>::zero(cs); 8];
    let two_pow_16 = Num::allocated_constant(cs, F::from_u64_unchecked(2u32.pow(16) as u64));
    for (dst, src) in limbs.iter_mut().zip(elem.limbs.array_chunks_mut::<2>()) {
        let low = Num::from_variable(src[0]);
        let high = Num::from_variable(src[1]);
        *dst = unsafe {
            UInt32::from_variable_unchecked(
                Num::fma(cs, &high, &two_pow_16, &F::ONE, &low, &F::ONE).get_variable(),
            )
        };
    }

    UInt256 { inner: limbs }
}

fn width_4_windowed_multiplication<F: SmallField, CS: ConstraintSystem<F>>(
    cs: &mut CS,
    mut point: SWProjectivePoint<F, Secp256Affine, Secp256BaseNNField<F>>,
    mut scalar: Secp256ScalarNNField<F>,
    base_field_params: &Arc<Secp256BaseNNFieldParams>,
    scalar_field_params: &Arc<Secp256ScalarNNFieldParams>,
) -> SWProjectivePoint<F, Secp256Affine, Secp256BaseNNField<F>> {
    scalar.enforce_reduced(cs);

    let beta = Secp256Fq::from_str(BETA).unwrap();
    let mut beta = Secp256BaseNNField::allocated_constant(cs, beta, &base_field_params);

    let bigint_from_hex_str = |cs: &mut CS, s: &str| -> UInt512<F> {
        let v = U256::from_str_radix(s, 16).unwrap();
        UInt512::allocated_constant(cs, (v, U256::zero()))
    };

    let modulus_minus_one_div_two = bigint_from_hex_str(cs, MODULUS_MINUS_ONE_DIV_TWO);

    let u256_from_hex_str = |cs: &mut CS, s: &str| -> UInt256<F> {
        let v = U256::from_str_radix(s, 16).unwrap();
        UInt256::allocated_constant(cs, v)
    };

    let a1 = u256_from_hex_str(cs, A1);
    let b1 = u256_from_hex_str(cs, B1);
    let a2 = u256_from_hex_str(cs, A2);
    let b2 = a1.clone();

    let boolean_false = Boolean::allocated_constant(cs, false);

    // Scalar decomposition
    let (k1_was_negated, k1, k2_was_negated, k2) = {
        let k = convert_field_element_to_uint256(cs, scalar.clone());

        // We take 8 non-zero limbs for the scalar (since it could be of any size), and 4 for B2
        // (since it fits in 128 bits).
        let b2_times_k = k.widening_mul(cs, &b2, 8, 4);
        // can not overflow u512
        let (b2_times_k, of) = b2_times_k.overflowing_add(cs, &modulus_minus_one_div_two);
        Boolean::enforce_equal(cs, &of, &boolean_false);
        let c1 = b2_times_k.to_high();

        // We take 8 non-zero limbs for the scalar (since it could be of any size), and 4 for B1
        // (since it fits in 128 bits).
        let b1_times_k = k.widening_mul(cs, &b1, 8, 4);
        // can not overflow u512
        let (b1_times_k, of) = b1_times_k.overflowing_add(cs, &modulus_minus_one_div_two);
        Boolean::enforce_equal(cs, &of, &boolean_false);
        let c2 = b1_times_k.to_high();

        let mut a1 = convert_uint256_to_field_element(cs, &a1, &scalar_field_params);
        let mut b1 = convert_uint256_to_field_element(cs, &b1, &scalar_field_params);
        let mut a2 = convert_uint256_to_field_element(cs, &a2, &scalar_field_params);
        let mut b2 = a1.clone();
        let mut c1 = convert_uint256_to_field_element(cs, &c1, &scalar_field_params);
        let mut c2 = convert_uint256_to_field_element(cs, &c2, &scalar_field_params);

        let mut c1_times_a1 = c1.mul(cs, &mut a1);
        let mut c2_times_a2 = c2.mul(cs, &mut a2);
        let mut k1 = scalar.sub(cs, &mut c1_times_a1).sub(cs, &mut c2_times_a2);
        k1.normalize(cs);
        let mut c2_times_b2 = c2.mul(cs, &mut b2);
        let mut k2 = c1.mul(cs, &mut b1).sub(cs, &mut c2_times_b2);
        k2.normalize(cs);

        let k1_u256 = convert_field_element_to_uint256(cs, k1.clone());
        let k2_u256 = convert_field_element_to_uint256(cs, k2.clone());
        let max_k1_or_k2 = UInt256::allocated_constant(cs, MAX_DECOMPOSITION_VALUE);
        // we will need k1 and k2 to be < 2^128, so we can compare via subtraction
        let (_res, k1_out_of_range) = max_k1_or_k2.overflowing_sub(cs, &k1_u256);
        let k1_negated = k1.negated(cs);
        // dbg!(k1.witness_hook(cs)());
        // dbg!(k1_negated.witness_hook(cs)());
        let k1 = <Secp256ScalarNNField<F> as NonNativeField<F, Secp256Fr>>::conditionally_select(
            cs,
            k1_out_of_range,
            &k1_negated,
            &k1,
        );
        let (_res, k2_out_of_range) = max_k1_or_k2.overflowing_sub(cs, &k2_u256);
        let k2_negated = k2.negated(cs);
        // dbg!(k2.witness_hook(cs)());
        // dbg!(k2_negated.witness_hook(cs)());
        let k2 = <Secp256ScalarNNField<F> as NonNativeField<F, Secp256Fr>>::conditionally_select(
            cs,
            k2_out_of_range,
            &k2_negated,
            &k2,
        );

        (k1_out_of_range, k1, k2_out_of_range, k2)
    };

    // dbg!(k1.witness_hook(cs)());
    // dbg!(k2.witness_hook(cs)());
    // dbg!(k1_was_negated.witness_hook(cs)());
    // dbg!(k2_was_negated.witness_hook(cs)());

    // create precomputed table of size 1<<4 - 1
    // there is no 0 * P in the table, we will handle it below
    let mut table = Vec::with_capacity(PRECOMPUTATION_TABLE_SIZE);
    let mut tmp = point.clone();
    let (mut p_affine, _) = point.convert_to_affine_or_default(cs, Secp256Affine::one());
    table.push(p_affine.clone());
    for _ in 1..PRECOMPUTATION_TABLE_SIZE {
        // 2P, 3P, ...
        tmp = tmp.add_mixed(cs, &mut p_affine);
        let (affine, _) = tmp.convert_to_affine_or_default(cs, Secp256Affine::one());
        table.push(affine);
    }
    assert_eq!(table.len(), PRECOMPUTATION_TABLE_SIZE);

    let mut endomorphisms_table = table.clone();
    for (x, _) in endomorphisms_table.iter_mut() {
        *x = x.mul(cs, &mut beta);
    }

    // we also know that we will multiply k1 by points, and k2 by their endomorphisms, and if they
    // were negated above to fit into range, we negate bases here
    for (_, y) in table.iter_mut() {
        let negated = y.negated(cs);
        *y = Selectable::conditionally_select(cs, k1_was_negated, &negated, &*y);
    }

    for (_, y) in endomorphisms_table.iter_mut() {
        let negated = y.negated(cs);
        *y = Selectable::conditionally_select(cs, k2_was_negated, &negated, &*y);
    }

    // now decompose every scalar we are interested in
    let k1_msb_decomposition = to_width_4_window_form(cs, k1);
    let k2_msb_decomposition = to_width_4_window_form(cs, k2);

    let mut comparison_constants = Vec::with_capacity(PRECOMPUTATION_TABLE_SIZE);
    for i in 1..=PRECOMPUTATION_TABLE_SIZE {
        let constant = Num::allocated_constant(cs, F::from_u64_unchecked(i as u64));
        comparison_constants.push(constant);
    }

    // now we do amortized double and add
    let mut acc = SWProjectivePoint::zero(cs, base_field_params);
    assert_eq!(k1_msb_decomposition.len(), NUM_MULTIPLICATION_STEPS_FOR_WIDTH_4);
    assert_eq!(k2_msb_decomposition.len(), NUM_MULTIPLICATION_STEPS_FOR_WIDTH_4);

    for (idx, (k1_window_idx, k2_window_idx)) in k1_msb_decomposition
        .into_iter()
        .zip(k2_msb_decomposition.into_iter())
        .enumerate()
    {
        let ignore_k1_part = k1_window_idx.is_zero(cs);
        let ignore_k2_part = k2_window_idx.is_zero(cs);

        // dbg!(k1_window_idx.witness_hook(cs)());
        // dbg!(k2_window_idx.witness_hook(cs)());
        // dbg!(ignore_k1_part.witness_hook(cs)());
        // dbg!(ignore_k2_part.witness_hook(cs)());

        let (mut selected_k1_part_x, mut selected_k1_part_y) = table[0].clone();
        let (mut selected_k2_part_x, mut selected_k2_part_y) = endomorphisms_table[0].clone();
        for i in 1..PRECOMPUTATION_TABLE_SIZE {
            let should_select_k1 = Num::equals(cs, &comparison_constants[i], &k1_window_idx);
            let should_select_k2 = Num::equals(cs, &comparison_constants[i], &k2_window_idx);
            selected_k1_part_x = Selectable::conditionally_select(
                cs,
                should_select_k1,
                &table[i].0,
                &selected_k1_part_x,
            );
            selected_k1_part_y = Selectable::conditionally_select(
                cs,
                should_select_k1,
                &table[i].1,
                &selected_k1_part_y,
            );
            selected_k2_part_x = Selectable::conditionally_select(
                cs,
                should_select_k2,
                &endomorphisms_table[i].0,
                &selected_k2_part_x,
            );
            selected_k2_part_y = Selectable::conditionally_select(
                cs,
                should_select_k2,
                &endomorphisms_table[i].1,
                &selected_k2_part_y,
            );
        }

        // dbg!(selected_k1_part_x.witness_hook(cs)());
        // dbg!(selected_k1_part_y.witness_hook(cs)());

        let tmp_acc = acc.add_mixed(cs, &mut (selected_k1_part_x, selected_k1_part_y));
        acc = Selectable::conditionally_select(cs, ignore_k1_part, &acc, &tmp_acc);
        let tmp_acc = acc.add_mixed(cs, &mut (selected_k2_part_x, selected_k2_part_y));
        acc = Selectable::conditionally_select(cs, ignore_k2_part, &acc, &tmp_acc);

        // let ((x, y), _) = acc.convert_to_affine_or_default(cs, Secp256Affine::zero());
        // dbg!(x.witness_hook(cs)());
        // dbg!(y.witness_hook(cs)());

        if idx != NUM_MULTIPLICATION_STEPS_FOR_WIDTH_4 - 1 {
            for _ in 0..WINDOW_WIDTH {
                acc = acc.double(cs);
            }
        }
    }

    acc
}

fn to_width_4_window_form<F: SmallField, CS: ConstraintSystem<F>>(
    cs: &mut CS,
    mut limited_width_scalar: Secp256ScalarNNField<F>,
) -> Vec<Num<F>> {
    limited_width_scalar.enforce_reduced(cs);
    // we know that width is 128 bits, so just do BE decomposition and put into resulting array
    let zero_num = Num::zero(cs);
    for word in limited_width_scalar.limbs[9..].iter() {
        let word = Num::from_variable(*word);
        Num::enforce_equal(cs, &word, &zero_num);
    }

    let byte_split_id = cs
        .get_table_id_for_marker::<ByteSplitTable<4>>()
        .expect("table should exist");
    let mut result = Vec::with_capacity(32);
    // special case
    {
        let highest_word = limited_width_scalar.limbs[8];
        let word = unsafe { UInt16::from_variable_unchecked(highest_word) };
        let [high, low] = word.to_be_bytes(cs);
        Num::enforce_equal(cs, &high.into_num(), &zero_num);
        let [l, h] = cs.perform_lookup::<1, 2>(byte_split_id, &[low.get_variable()]);
        Num::enforce_equal(cs, &Num::from_variable(h), &zero_num);
        let l = Num::from_variable(l);
        result.push(l);
    }

    for word in limited_width_scalar.limbs[..8].iter().rev() {
        let word = unsafe { UInt16::from_variable_unchecked(*word) };
        let [high, low] = word.to_be_bytes(cs);
        for t in [high, low].into_iter() {
            let [l, h] = cs.perform_lookup::<1, 2>(byte_split_id, &[t.get_variable()]);
            let h = Num::from_variable(h);
            let l = Num::from_variable(l);
            result.push(h);
            result.push(l);
        }
    }
    assert_eq!(result.len(), NUM_MULTIPLICATION_STEPS_FOR_WIDTH_4);

    result
}

pub(crate) fn fixed_base_mul<
    F: SmallField,
    CS: ConstraintSystem<F>,
    NNS: boojum::pairing::ff::PrimeField,
    NNB: boojum::pairing::ff::PrimeField + boojum::pairing::ff::SqrtField,
    NNC: boojum::pairing::GenericCurveAffine<Base = NNB>,
    const N: usize,
>(
    cs: &mut CS,
    mut scalar: NonNativeFieldOverU16<F, NNS, N>,
    base_field_params: &Arc<NonNativeFieldOverU16Params<NNB, N>>,
    scalar_canonical_limbs: usize,
    base_canonical_limbs_canonical_limbs: usize,
    fixed_base_table_ids: &[[u32; 8]],
) -> SWProjectivePoint<F, NNC, NonNativeFieldOverU16<F, NNB, N>>
where
    [(); N + 1]:,
{
    assert!(base_canonical_limbs_canonical_limbs % 2 == 0);
    assert!(scalar_canonical_limbs % 2 == 0);
    assert_eq!(scalar_canonical_limbs * 2, fixed_base_table_ids.len());
    assert_eq!(base_canonical_limbs_canonical_limbs / 2, 8);

    scalar.enforce_reduced(cs);
    let is_zero = scalar.is_zero(cs);
    let bytes = scalar
        .limbs
        .iter()
        .take(scalar_canonical_limbs)
        .flat_map(|el| unsafe { UInt16::from_variable_unchecked(*el).to_le_bytes(cs) })
        .collect::<Vec<UInt8<F>>>();

    let zero_point =
        SWProjectivePoint::<F, NNC, NonNativeFieldOverU16<F, NNB, N>>::zero(cs, base_field_params);
    let mut acc =
        SWProjectivePoint::<F, NNC, NonNativeFieldOverU16<F, NNB, N>>::zero(cs, base_field_params);

    fixed_base_table_ids
        .iter()
        .copied()
        .zip(bytes)
        .rev()
        .for_each(|(ids, byte)| {
            let (x, y): (Vec<Variable>, Vec<Variable>) = ids
                .iter()
                .flat_map(|id| {
                    let [x_v, y_v] = cs.perform_lookup::<1, 2>(*id, &[byte.get_variable()]);
                    let x_v = unsafe { UInt32::from_variable_unchecked(x_v) };
                    let y_v = unsafe { UInt32::from_variable_unchecked(y_v) };
                    let x_v = x_v.to_le_bytes(cs);
                    let y_v = y_v.to_le_bytes(cs);
                    let x_1 = UInt16::from_le_bytes(cs, x_v[..2].try_into().unwrap());
                    let x_2 = UInt16::from_le_bytes(cs, x_v[2..].try_into().unwrap());
                    let y_1 = UInt16::from_le_bytes(cs, y_v[..2].try_into().unwrap());
                    let y_2 = UInt16::from_le_bytes(cs, y_v[2..].try_into().unwrap());
                    [
                        (x_1.get_variable(), y_1.get_variable()),
                        (x_2.get_variable(), y_2.get_variable()),
                    ]
                })
                .collect::<Vec<(Variable, Variable)>>()
                .into_iter()
                .unzip();
            let zero_var = cs.allocate_constant(F::ZERO);
            let mut x_arr = [zero_var; N];
            x_arr[..base_canonical_limbs_canonical_limbs]
                .copy_from_slice(&x[..base_canonical_limbs_canonical_limbs]);
            let mut y_arr = [zero_var; N];
            y_arr[..base_canonical_limbs_canonical_limbs]
                .copy_from_slice(&y[..base_canonical_limbs_canonical_limbs]);
            let x = NonNativeFieldOverU16 {
                limbs: x_arr,
                non_zero_limbs: base_canonical_limbs_canonical_limbs,
                tracker: OverflowTracker { max_moduluses: 1 },
                form: RepresentationForm::Normalized,
                params: base_field_params.clone(),
                _marker: std::marker::PhantomData,
            };
            let y = NonNativeFieldOverU16 {
                limbs: y_arr,
                non_zero_limbs: base_canonical_limbs_canonical_limbs,
                tracker: OverflowTracker { max_moduluses: 1 },
                form: RepresentationForm::Normalized,
                params: base_field_params.clone(),
                _marker: std::marker::PhantomData,
            };
            let new_acc = acc.add_mixed(cs, &mut (x, y));
            let should_not_update = byte.is_zero(cs);
            acc = Selectable::conditionally_select(cs, should_not_update, &acc, &new_acc);
        });
    acc = Selectable::conditionally_select(cs, is_zero, &zero_point, &acc);
    acc
}

fn ecrecover_precompile_inner_routine<
    F: SmallField,
    CS: ConstraintSystem<F>,
    const MESSAGE_HASH_CAN_BE_ZERO: bool,
>(
    cs: &mut CS,
    recid: &UInt8<F>,
    r: &UInt256<F>,
    s: &UInt256<F>,
    message_hash: &UInt256<F>,
    valid_x_in_external_field: Secp256BaseNNField<F>,
    valid_y_in_external_field: Secp256BaseNNField<F>,
    valid_t_in_external_field: Secp256BaseNNField<F>,
    base_field_params: &Arc<Secp256BaseNNFieldParams>,
    scalar_field_params: &Arc<Secp256ScalarNNFieldParams>,
) -> (Boolean<F>, UInt256<F>) {
    use boojum::pairing::ff::Field;
    let curve_b = Secp256Affine::b_coeff();

    let mut minus_one = Secp256Fq::one();
    minus_one.negate();

    let mut curve_b_nn =
        Secp256BaseNNField::<F>::allocated_constant(cs, curve_b, &base_field_params);
    let mut minus_one_nn =
        Secp256BaseNNField::<F>::allocated_constant(cs, minus_one, &base_field_params);

    let secp_n_u256 = U256([
        scalar_field_params.modulus_u1024.as_ref().as_words()[0],
        scalar_field_params.modulus_u1024.as_ref().as_words()[1],
        scalar_field_params.modulus_u1024.as_ref().as_words()[2],
        scalar_field_params.modulus_u1024.as_ref().as_words()[3],
    ]);
    let secp_n_u256 = UInt256::allocated_constant(cs, secp_n_u256);

    let secp_p_u256 = U256([
        base_field_params.modulus_u1024.as_ref().as_words()[0],
        base_field_params.modulus_u1024.as_ref().as_words()[1],
        base_field_params.modulus_u1024.as_ref().as_words()[2],
        base_field_params.modulus_u1024.as_ref().as_words()[3],
    ]);
    let secp_p_u256 = UInt256::allocated_constant(cs, secp_p_u256);

    let mut exception_flags = ArrayVec::<_, EXCEPTION_FLAGS_ARR_LEN>::new();

    // recid = (x_overflow ? 2 : 0) | (secp256k1_fe_is_odd(&r.y) ? 1 : 0)
    // The point X = (x, y) we are going to recover is not known at the start, but it is strongly
    // related to r. This is because x = r + kn for some integer k, where x is an element of the
    // field F_q . In other words, x < q. (here n is the order of group of points on elleptic
    // curve) For secp256k1 curve values of q and n are relatively close, that is,
    // the probability of a random element of Fq being greater than n is about 1/{2^128}.
    // This in turn means that the overwhelming majority of r determine a unique x, however some of
    // them determine two: x = r and x = r + n. If x_overflow flag is set than x = r + n

    let [y_is_odd, x_overflow, ..] =
        Num::<F>::from_variable(recid.get_variable()).spread_into_bits::<_, 8>(cs);

    let (r_plus_n, of) = r.overflowing_add(cs, &secp_n_u256);
    let mut x_as_u256 = UInt256::conditionally_select(cs, x_overflow, &r_plus_n, &r);
    let error = Boolean::multi_and(cs, &[x_overflow, of]);
    exception_flags.push(error);

    // we handle x separately as it is the only element of base field of a curve (not a scalar field
    // element!) check that x < q - order of base point on Secp256 curve
    // if it is not actually the case - mask x to be zero
    let (_res, is_in_range) = x_as_u256.overflowing_sub(cs, &secp_p_u256);
    x_as_u256 = x_as_u256.mask(cs, is_in_range);
    let x_is_not_in_range = is_in_range.negated(cs);
    exception_flags.push(x_is_not_in_range);

    let mut x_fe = convert_uint256_to_field_element(cs, &x_as_u256, &base_field_params);

    let (mut r_fe, r_is_zero) =
        convert_uint256_to_field_element_masked(cs, &r, &scalar_field_params);
    exception_flags.push(r_is_zero);
    let (mut s_fe, s_is_zero) =
        convert_uint256_to_field_element_masked(cs, &s, &scalar_field_params);
    exception_flags.push(s_is_zero);

    let (mut message_hash_fe, message_hash_is_zero) = if MESSAGE_HASH_CAN_BE_ZERO {
        (
            convert_uint256_to_field_element(cs, &message_hash, scalar_field_params),
            Boolean::allocated_constant(cs, false),
        )
    } else {
        convert_uint256_to_field_element_masked(cs, &message_hash, scalar_field_params)
    };
    exception_flags.push(message_hash_is_zero);

    // curve equation is y^2 = x^3 + b
    // we compute t = r^3 + b and check if t is a quadratic residue or not.
    // we do this by computing Legendre symbol (t, p) = t^[(p-1)/2] (mod p)
    //           p = 2^256 - 2^32 - 2^9 - 2^8 - 2^7 - 2^6 - 2^4 - 1
    // n = (p-1)/2 = 2^255 - 2^31 - 2^8 - 2^7 - 2^6 - 2^5 - 2^3 - 1
    // we have to compute t^b = t^{2^255} / ( t^{2^31} * t^{2^8} * t^{2^7} * t^{2^6} * t^{2^5} *
    // t^{2^3} * t) if t is not a quadratic residue we return error and replace x by another
    // value that will make t = x^3 + b a quadratic residue

    let mut t = x_fe.square(cs);
    t = t.mul(cs, &mut x_fe);
    t = t.add(cs, &mut curve_b_nn);

    let t_is_zero = t.is_zero(cs);
    exception_flags.push(t_is_zero);

    // if t is zero then just mask
    let t = Selectable::conditionally_select(cs, t_is_zero, &valid_t_in_external_field, &t);

    // array of powers of t of the form t^{2^i} starting from i = 0 to 255
    let mut t_powers = Vec::with_capacity(X_POWERS_ARR_LEN);
    t_powers.push(t);

    for _ in 1..X_POWERS_ARR_LEN {
        let prev = t_powers.last_mut().unwrap();
        let next = prev.square(cs);
        t_powers.push(next);
    }

    let mut acc = t_powers[0].clone();
    for idx in [3, 5, 6, 7, 8, 31].into_iter() {
        let other = &mut t_powers[idx];
        acc = acc.mul(cs, other);
    }
    let mut legendre_symbol = t_powers[255].div_unchecked(cs, &mut acc);

    // we can also reuse the same values to compute square root in case of p = 3 mod 4
    //           p = 2^256 - 2^32 - 2^9 - 2^8 - 2^7 - 2^6 - 2^4 - 1
    // n = (p+1)/4 = 2^254 - 2^30 - 2^7 - 2^6 - 2^5 - 2^4 - 2^2

    let mut acc_2 = t_powers[2].clone();
    for idx in [4, 5, 6, 7, 30].into_iter() {
        let other = &mut t_powers[idx];
        acc_2 = acc_2.mul(cs, other);
    }

    let mut may_be_recovered_y = t_powers[254].div_unchecked(cs, &mut acc_2);
    may_be_recovered_y.normalize(cs);
    let may_be_recovered_y_negated = may_be_recovered_y.negated(cs);

    if crate::config::CIRCUIT_VERSOBE {
        dbg!(may_be_recovered_y.witness_hook(cs)());
        dbg!(may_be_recovered_y_negated.witness_hook(cs)());
    }

    let [lowest_bit, ..] =
        Num::<F>::from_variable(may_be_recovered_y.limbs[0]).spread_into_bits::<_, 16>(cs);

    // if lowest bit != parity bit, then we need conditionally select
    let should_swap = lowest_bit.xor(cs, y_is_odd);
    let may_be_recovered_y = Selectable::conditionally_select(
        cs,
        should_swap,
        &may_be_recovered_y_negated,
        &may_be_recovered_y,
    );

    let t_is_nonresidue =
        Secp256BaseNNField::<F>::equals(cs, &mut legendre_symbol, &mut minus_one_nn);
    exception_flags.push(t_is_nonresidue);
    // unfortunately, if t is found to be a quadratic nonresidue, we can't simply let x to be zero,
    // because then t_new = 7 is again a quadratic nonresidue. So, in this case we let x to be 9,
    // then t = 16 is a quadratic residue
    let x =
        Selectable::conditionally_select(cs, t_is_nonresidue, &valid_x_in_external_field, &x_fe);
    let y = Selectable::conditionally_select(
        cs,
        t_is_nonresidue,
        &valid_y_in_external_field,
        &may_be_recovered_y,
    );

    // we recovered (x, y) using curve equation, so it's on curve (or was masked)
    let mut r_fe_inversed = r_fe.inverse_unchecked(cs);
    let mut s_by_r_inv = s_fe.mul(cs, &mut r_fe_inversed);
    let mut message_hash_by_r_inv = message_hash_fe.mul(cs, &mut r_fe_inversed);

    s_by_r_inv.normalize(cs);
    let mut message_hash_by_r_inv_negated = message_hash_by_r_inv.negated(cs);
    message_hash_by_r_inv_negated.normalize(cs);

    // now we are going to compute the public key Q = (x, y) determined by the formula:
    // Q = (s * X - hash * G) / r which is equivalent to r * Q = s * X - hash * G

    if crate::config::CIRCUIT_VERSOBE {
        dbg!(x.witness_hook(cs)());
        dbg!(y.witness_hook(cs)());
        dbg!(s_by_r_inv.witness_hook(cs)());
        dbg!(message_hash_by_r_inv_negated.witness_hook(cs)());
    }

    let recovered_point =
        SWProjectivePoint::<F, Secp256Affine, Secp256BaseNNField<F>>::from_xy_unchecked(cs, x, y);

    // now we do multiplication
    let mut s_times_x = width_4_windowed_multiplication(
        cs,
        recovered_point.clone(),
        s_by_r_inv.clone(),
        &base_field_params,
        &scalar_field_params,
    );

    let mut full_table_ids = vec![];
    seq_macro::seq!(C in 0..32 {
        let ids = [
            cs.get_table_id_for_marker::<FixedBaseMulTable<0, C>>()
                .expect("table must exist"),
            cs.get_table_id_for_marker::<FixedBaseMulTable<1, C>>()
                .expect("table must exist"),
            cs.get_table_id_for_marker::<FixedBaseMulTable<2, C>>()
                .expect("table must exist"),
            cs.get_table_id_for_marker::<FixedBaseMulTable<3, C>>()
                .expect("table must exist"),
            cs.get_table_id_for_marker::<FixedBaseMulTable<4, C>>()
                .expect("table must exist"),
            cs.get_table_id_for_marker::<FixedBaseMulTable<5, C>>()
                .expect("table must exist"),
            cs.get_table_id_for_marker::<FixedBaseMulTable<6, C>>()
                .expect("table must exist"),
            cs.get_table_id_for_marker::<FixedBaseMulTable<7, C>>()
                .expect("table must exist"),
        ];
        full_table_ids.push(ids);
    });

    let mut hash_times_g = fixed_base_mul::<F, CS, Secp256Fr, Secp256Fq, Secp256Affine, 17>(
        cs,
        message_hash_by_r_inv_negated,
        &base_field_params,
        SCALAR_FIELD_CANONICAL_REPR_LIMBS,
        BASE_FIELD_CANONICAL_REPR_LIMBS,
        &full_table_ids,
    );

    let (mut q_acc, is_infinity) =
        hash_times_g.convert_to_affine_or_default(cs, Secp256Affine::one());
    let q_acc_added = s_times_x.add_mixed(cs, &mut q_acc);
    let mut q_acc = Selectable::conditionally_select(cs, is_infinity, &s_times_x, &q_acc_added);

    let ((q_x, q_y), is_infinity) = q_acc.convert_to_affine_or_default(cs, Secp256Affine::one());
    exception_flags.push(is_infinity);
    let any_exception = Boolean::multi_or(cs, &exception_flags[..]);

    let zero_u8 = UInt8::zero(cs);

    if crate::config::CIRCUIT_VERSOBE {
        dbg!(q_x.witness_hook(cs)());
        dbg!(q_y.witness_hook(cs)());
    }

    let mut bytes_to_hash = [zero_u8; 64];
    let it = q_x.limbs[..16]
        .iter()
        .rev()
        .chain(q_y.limbs[..16].iter().rev());

    for (dst, src) in bytes_to_hash.array_chunks_mut::<2>().zip(it) {
        let limb = unsafe { UInt16::from_variable_unchecked(*src) };
        *dst = limb.to_be_bytes(cs);
    }

    let mut digest_bytes = keccak256(cs, &bytes_to_hash);
    // digest is 32 bytes, but we need only 20 to recover address
    digest_bytes[0..12].copy_from_slice(&[zero_u8; 12]); // empty out top bytes
    digest_bytes.reverse();
    let written_value_unmasked = UInt256::from_le_bytes(cs, digest_bytes);

    let written_value = written_value_unmasked.mask_negated(cs, any_exception);
    let all_ok = any_exception.negated(cs);

    (all_ok, written_value)
}

pub fn ecrecover_function_entry_point<
    F: SmallField,
    CS: ConstraintSystem<F>,
    R: CircuitRoundFunction<F, 8, 12, 4> + AlgebraicRoundFunction<F, 8, 12, 4>,
>(
    cs: &mut CS,
    witness: EcrecoverCircuitInstanceWitness<F>,
    round_function: &R,
    limit: usize,
) -> [Num<F>; INPUT_OUTPUT_COMMITMENT_LENGTH]
where
    [(); <LogQuery<F> as CSAllocatableExt<F>>::INTERNAL_STRUCT_LEN]:,
    [(); <MemoryQuery<F> as CSAllocatableExt<F>>::INTERNAL_STRUCT_LEN]:,
    [(); <UInt256<F> as CSAllocatableExt<F>>::INTERNAL_STRUCT_LEN]:,
    [(); <UInt256<F> as CSAllocatableExt<F>>::INTERNAL_STRUCT_LEN + 1]:,
{
    assert!(limit <= u32::MAX as usize);

    let EcrecoverCircuitInstanceWitness {
        closed_form_input,
        requests_queue_witness,
        memory_reads_witness,
    } = witness;

    let memory_reads_witness: VecDeque<_> = memory_reads_witness.into_iter().flatten().collect();

    let precompile_address = UInt160::allocated_constant(
        cs,
        *zkevm_opcode_defs::system_params::ECRECOVER_INNER_FUNCTION_PRECOMPILE_FORMAL_ADDRESS,
    );
    let aux_byte_for_precompile = UInt8::allocated_constant(cs, PRECOMPILE_AUX_BYTE);

    let scalar_params = Arc::new(secp256k1_scalar_field_params());
    let base_params = Arc::new(secp256k1_base_field_params());

    let valid_x_in_external_field = Secp256BaseNNField::allocated_constant(
        cs,
        Secp256Fq::from_str(&VALID_X_CUBED_IN_EXTERNAL_FIELD.to_string()).unwrap(),
        &base_params,
    );
    let valid_t_in_external_field = Secp256BaseNNField::allocated_constant(
        cs,
        Secp256Fq::from_str(&(VALID_X_CUBED_IN_EXTERNAL_FIELD + SECP_B_COEF).to_string()).unwrap(),
        &base_params,
    );
    let valid_y_in_external_field = Secp256BaseNNField::allocated_constant(
        cs,
        Secp256Fq::from_str(&VALID_Y_IN_EXTERNAL_FIELD.to_string()).unwrap(),
        &base_params,
    );

    let mut structured_input =
        EcrecoverCircuitInputOutput::alloc_ignoring_outputs(cs, closed_form_input.clone());
    let start_flag = structured_input.start_flag;

    let requests_queue_state_from_input = structured_input.observable_input.initial_log_queue_state;

    // it must be trivial
    requests_queue_state_from_input.enforce_trivial_head(cs);

    let requests_queue_state_from_fsm = structured_input.hidden_fsm_input.log_queue_state;

    let requests_queue_state = QueueState::conditionally_select(
        cs,
        start_flag,
        &requests_queue_state_from_input,
        &requests_queue_state_from_fsm,
    );

    let memory_queue_state_from_input =
        structured_input.observable_input.initial_memory_queue_state;

    // it must be trivial
    memory_queue_state_from_input.enforce_trivial_head(cs);

    let memory_queue_state_from_fsm = structured_input.hidden_fsm_input.memory_queue_state;

    let memory_queue_state = QueueState::conditionally_select(
        cs,
        start_flag,
        &memory_queue_state_from_input,
        &memory_queue_state_from_fsm,
    );

    let mut requests_queue = StorageLogQueue::<F, R>::from_state(cs, requests_queue_state);
    let queue_witness = CircuitQueueWitness::from_inner_witness(requests_queue_witness);
    requests_queue.witness = Arc::new(queue_witness);

    let mut memory_queue = MemoryQueue::<F, R>::from_state(cs, memory_queue_state);

    let one_u32 = UInt32::allocated_constant(cs, 1u32);
    let zero_u256 = UInt256::zero(cs);
    let boolean_false = Boolean::allocated_constant(cs, false);
    let boolean_true = Boolean::allocated_constant(cs, true);

    use crate::storage_application::ConditionalWitnessAllocator;
    let read_queries_allocator = ConditionalWitnessAllocator::<F, UInt256<F>> {
        witness_source: Arc::new(RwLock::new(memory_reads_witness)),
    };

    for _cycle in 0..limit {
        let is_empty = requests_queue.is_empty(cs);
        let should_process = is_empty.negated(cs);
        let (request, _) = requests_queue.pop_front(cs, should_process);

        let mut precompile_call_params =
            EcrecoverPrecompileCallParams::from_encoding(cs, request.key);

        let timestamp_to_use_for_read = request.timestamp;
        let timestamp_to_use_for_write = timestamp_to_use_for_read.add_no_overflow(cs, one_u32);

        Num::conditionally_enforce_equal(
            cs,
            should_process,
            &Num::from_variable(request.aux_byte.get_variable()),
            &Num::from_variable(aux_byte_for_precompile.get_variable()),
        );
        for (a, b) in request
            .address
            .inner
            .iter()
            .zip(precompile_address.inner.iter())
        {
            Num::conditionally_enforce_equal(
                cs,
                should_process,
                &Num::from_variable(a.get_variable()),
                &Num::from_variable(b.get_variable()),
            );
        }

        let mut read_values = [zero_u256; NUM_MEMORY_READS_PER_CYCLE];
        let mut bias_variable = should_process.get_variable();
        for dst in read_values.iter_mut() {
            let read_query_value: UInt256<F> = read_queries_allocator
                .conditionally_allocate_biased(cs, should_process, bias_variable);
            bias_variable = read_query_value.inner[0].get_variable();

            *dst = read_query_value;

            let read_query = MemoryQuery {
                timestamp: timestamp_to_use_for_read,
                memory_page: precompile_call_params.input_page,
                index: precompile_call_params.input_offset,
                rw_flag: boolean_false,
                is_ptr: boolean_false,
                value: read_query_value,
            };

            let _ = memory_queue.push(cs, read_query, should_process);

            precompile_call_params.input_offset = precompile_call_params
                .input_offset
                .add_no_overflow(cs, one_u32);
        }

        let [message_hash_as_u256, v_as_u256, r_as_u256, s_as_u256] = read_values;
        let rec_id = v_as_u256.inner[0].to_le_bytes(cs)[0];

        if crate::config::CIRCUIT_VERSOBE {
            if should_process.witness_hook(cs)().unwrap() == true {
                dbg!(rec_id.witness_hook(cs)());
                dbg!(r_as_u256.witness_hook(cs)());
                dbg!(s_as_u256.witness_hook(cs)());
                dbg!(message_hash_as_u256.witness_hook(cs)());
            }
        }

        let (success, written_value) = ecrecover_precompile_inner_routine::<_, _, ALLOW_ZERO_MESSAGE>(
            cs,
            &rec_id,
            &r_as_u256,
            &s_as_u256,
            &message_hash_as_u256,
            valid_x_in_external_field.clone(),
            valid_y_in_external_field.clone(),
            valid_t_in_external_field.clone(),
            &base_params,
            &scalar_params,
        );

        let success_as_u32 = unsafe { UInt32::from_variable_unchecked(success.get_variable()) };
        let mut success_as_u256 = zero_u256;
        success_as_u256.inner[0] = success_as_u32;

        if crate::config::CIRCUIT_VERSOBE {
            if should_process.witness_hook(cs)().unwrap() == true {
                dbg!(success_as_u256.witness_hook(cs)());
                dbg!(written_value.witness_hook(cs)());
            }
        }

        let success_query = MemoryQuery {
            timestamp: timestamp_to_use_for_write,
            memory_page: precompile_call_params.output_page,
            index: precompile_call_params.output_offset,
            rw_flag: boolean_true,
            value: success_as_u256,
            is_ptr: boolean_false,
        };

        precompile_call_params.output_offset = precompile_call_params
            .output_offset
            .add_no_overflow(cs, one_u32);

        let _ = memory_queue.push(cs, success_query, should_process);

        let value_query = MemoryQuery {
            timestamp: timestamp_to_use_for_write,
            memory_page: precompile_call_params.output_page,
            index: precompile_call_params.output_offset,
            rw_flag: boolean_true,
            value: written_value,
            is_ptr: boolean_false,
        };

        let _ = memory_queue.push(cs, value_query, should_process);
    }

    requests_queue.enforce_consistency(cs);

    // form the final state
    let done = requests_queue.is_empty(cs);
    structured_input.completion_flag = done;
    structured_input.observable_output = PrecompileFunctionOutputData::placeholder(cs);

    let final_memory_state = memory_queue.into_state();
    let final_requets_state = requests_queue.into_state();

    structured_input.observable_output.final_memory_state = QueueState::conditionally_select(
        cs,
        structured_input.completion_flag,
        &final_memory_state,
        &structured_input.observable_output.final_memory_state,
    );

    structured_input.hidden_fsm_output.log_queue_state = final_requets_state;
    structured_input.hidden_fsm_output.memory_queue_state = final_memory_state;

    // self-check
    structured_input.hook_compare_witness(cs, &closed_form_input);

    use boojum::cs::gates::PublicInputGate;

    let compact_form =
        ClosedFormInputCompactForm::from_full_form(cs, &structured_input, round_function);
    let input_commitment = commit_variable_length_encodable_item(cs, &compact_form, round_function);
    for el in input_commitment.iter() {
        let gate = PublicInputGate::new(el.get_variable());
        gate.add_to_cs(cs);
    }

    input_commitment
}

#[cfg(test)]
mod test {
    use boojum::{
        field::goldilocks::GoldilocksField,
        gadgets::traits::allocatable::CSAllocatable,
        pairing::ff::{Field, PrimeField},
        worker::Worker,
    };

    use super::*;

    type F = GoldilocksField;
    type P = GoldilocksField;

    use boojum::{
        config::DevCSConfig,
        pairing::{ff::PrimeFieldRepr, GenericCurveAffine, GenericCurveProjective},
    };
    use rand::{Rng, SeedableRng, XorShiftRng};

    pub fn deterministic_rng() -> XorShiftRng {
        XorShiftRng::from_seed([0x5dbe6259, 0x8d313d76, 0x3237db17, 0xe5bc0654])
    }

    fn simulate_signature() -> (Secp256Fr, Secp256Fr, Secp256Affine, Secp256Fr) {
        let mut rng = deterministic_rng();
        let sk: Secp256Fr = rng.gen();

        simulate_signature_for_sk(sk)
    }

    fn transmute_representation<T: PrimeFieldRepr, U: PrimeFieldRepr>(repr: T) -> U {
        assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<U>());

        unsafe { std::mem::transmute_copy::<T, U>(&repr) }
    }

    fn simulate_signature_for_sk(
        sk: Secp256Fr,
    ) -> (Secp256Fr, Secp256Fr, Secp256Affine, Secp256Fr) {
        let mut rng = deterministic_rng();
        let pk = Secp256Affine::one().mul(sk.into_repr()).into_affine();
        let digest: Secp256Fr = rng.gen();
        let k: Secp256Fr = rng.gen();
        let r_point = Secp256Affine::one().mul(k.into_repr()).into_affine();

        let r_x = r_point.into_xy_unchecked().0;
        let r = transmute_representation::<_, <Secp256Fr as PrimeField>::Repr>(r_x.into_repr());
        let r = Secp256Fr::from_repr(r).unwrap();

        let k_inv = k.inverse().unwrap();
        let mut s = r;
        s.mul_assign(&sk);
        s.add_assign(&digest);
        s.mul_assign(&k_inv);

        {
            let mut mul_by_generator = digest;
            mul_by_generator.mul_assign(&r.inverse().unwrap());
            mul_by_generator.negate();

            let mut mul_by_r = s;
            mul_by_r.mul_assign(&r.inverse().unwrap());

            let res_1 = Secp256Affine::one().mul(mul_by_generator.into_repr());
            let res_2 = r_point.mul(mul_by_r.into_repr());

            let mut tmp = res_1;
            tmp.add_assign(&res_2);

            let tmp = tmp.into_affine();

            let x = tmp.into_xy_unchecked().0;
            assert_eq!(x, pk.into_xy_unchecked().0);
        }

        (r, s, pk, digest)
    }

    fn repr_into_u256<T: PrimeFieldRepr>(repr: T) -> U256 {
        let mut u256 = U256::zero();
        u256.0.copy_from_slice(&repr.as_ref()[..4]);

        u256
    }

    use boojum::{
        cs::{
            cs_builder::*, cs_builder_reference::CsReferenceImplementationBuilder, gates::*,
            implementations::reference_cs::CSReferenceImplementation,
            traits::gate::GatePlacementStrategy, CSGeometry, *,
        },
        gadgets::tables::{byte_split::ByteSplitTable, *},
    };

    use crate::ecrecover::secp256k1::fixed_base_mul_table::{
        create_fixed_base_mul_table, FixedBaseMulTable,
    };

    fn create_cs(
        max_trace_len: usize,
    ) -> CSReferenceImplementation<
        F,
        P,
        DevCSConfig,
        impl GateConfigurationHolder<F>,
        impl StaticToolboxHolder,
    > {
        let geometry = CSGeometry {
            num_columns_under_copy_permutation: 100,
            num_witness_columns: 0,
            num_constant_columns: 8,
            max_allowed_constraint_degree: 4,
        };
        let max_variables = 1 << 26;

        fn configure<
            F: SmallField,
            T: CsBuilderImpl<F, T>,
            GC: GateConfigurationHolder<F>,
            TB: StaticToolboxHolder,
        >(
            builder: CsBuilder<T, F, GC, TB>,
        ) -> CsBuilder<T, F, impl GateConfigurationHolder<F>, impl StaticToolboxHolder> {
            let builder = builder.allow_lookup(
                LookupParameters::UseSpecializedColumnsWithTableIdAsConstant {
                    width: 3,
                    num_repetitions: 8,
                    share_table_id: true,
                },
            );
            let builder = U8x4FMAGate::configure_builder(
                builder,
                GatePlacementStrategy::UseGeneralPurposeColumns,
            );
            let builder = ConstantsAllocatorGate::configure_builder(
                builder,
                GatePlacementStrategy::UseGeneralPurposeColumns,
            );
            let builder = FmaGateInBaseFieldWithoutConstant::configure_builder(
                builder,
                GatePlacementStrategy::UseGeneralPurposeColumns,
            );
            let builder = ReductionGate::<F, 4>::configure_builder(
                builder,
                GatePlacementStrategy::UseGeneralPurposeColumns,
            );
            // let owned_cs = ReductionGate::<F, 4>::configure_for_cs(owned_cs,
            // GatePlacementStrategy::UseSpecializedColumns { num_repetitions: 8, share_constants:
            // true });
            let builder = BooleanConstraintGate::configure_builder(
                builder,
                GatePlacementStrategy::UseGeneralPurposeColumns,
            );
            let builder = UIntXAddGate::<32>::configure_builder(
                builder,
                GatePlacementStrategy::UseGeneralPurposeColumns,
            );
            let builder = UIntXAddGate::<16>::configure_builder(
                builder,
                GatePlacementStrategy::UseGeneralPurposeColumns,
            );
            let builder = UIntXAddGate::<8>::configure_builder(
                builder,
                GatePlacementStrategy::UseGeneralPurposeColumns,
            );
            let builder = SelectionGate::configure_builder(
                builder,
                GatePlacementStrategy::UseGeneralPurposeColumns,
            );
            let builder = ZeroCheckGate::configure_builder(
                builder,
                GatePlacementStrategy::UseGeneralPurposeColumns,
                false,
            );
            let builder = DotProductGate::<4>::configure_builder(
                builder,
                GatePlacementStrategy::UseGeneralPurposeColumns,
            );
            // let owned_cs = DotProductGate::<4>::configure_for_cs(owned_cs,
            // GatePlacementStrategy::UseSpecializedColumns { num_repetitions: 1, share_constants:
            // true });
            let builder = NopGate::configure_builder(
                builder,
                GatePlacementStrategy::UseGeneralPurposeColumns,
            );

            builder
        }

        let builder_impl =
            CsReferenceImplementationBuilder::<F, P, DevCSConfig>::new(geometry, max_trace_len);
        let builder = new_builder::<_, F>(builder_impl);

        let builder = configure(builder);
        let mut owned_cs = builder.build(max_variables);

        // add tables
        let table = create_xor8_table();
        owned_cs.add_lookup_table::<Xor8Table, 3>(table);

        let table = create_and8_table();
        owned_cs.add_lookup_table::<And8Table, 3>(table);

        // let table = create_naf_abs_div2_table();
        // owned_cs.add_lookup_table::<NafAbsDiv2Table, 3>(table);

        // let table = create_wnaf_decomp_table();
        // owned_cs.add_lookup_table::<WnafDecompTable, 3>(table);

        seq_macro::seq!(C in 0..32 {
            let table = create_fixed_base_mul_table::<F, 0, C>();
            owned_cs.add_lookup_table::<FixedBaseMulTable<0, C>, 3>(table);
            let table = create_fixed_base_mul_table::<F, 1, C>();
            owned_cs.add_lookup_table::<FixedBaseMulTable<1, C>, 3>(table);
            let table = create_fixed_base_mul_table::<F, 2, C>();
            owned_cs.add_lookup_table::<FixedBaseMulTable<2, C>, 3>(table);
            let table = create_fixed_base_mul_table::<F, 3, C>();
            owned_cs.add_lookup_table::<FixedBaseMulTable<3, C>, 3>(table);
            let table = create_fixed_base_mul_table::<F, 4, C>();
            owned_cs.add_lookup_table::<FixedBaseMulTable<4, C>, 3>(table);
            let table = create_fixed_base_mul_table::<F, 5, C>();
            owned_cs.add_lookup_table::<FixedBaseMulTable<5, C>, 3>(table);
            let table = create_fixed_base_mul_table::<F, 6, C>();
            owned_cs.add_lookup_table::<FixedBaseMulTable<6, C>, 3>(table);
            let table = create_fixed_base_mul_table::<F, 7, C>();
            owned_cs.add_lookup_table::<FixedBaseMulTable<7, C>, 3>(table);
        });

        let table = create_byte_split_table::<F, 1>();
        owned_cs.add_lookup_table::<ByteSplitTable<1>, 3>(table);
        let table = create_byte_split_table::<F, 2>();
        owned_cs.add_lookup_table::<ByteSplitTable<2>, 3>(table);
        let table = create_byte_split_table::<F, 3>();
        owned_cs.add_lookup_table::<ByteSplitTable<3>, 3>(table);
        let table = create_byte_split_table::<F, 4>();
        owned_cs.add_lookup_table::<ByteSplitTable<4>, 3>(table);

        owned_cs
    }

    #[test]
    fn test_fixed_base_mul() {
        let mut owned_cs = create_cs(1 << 21);
        let cs = &mut owned_cs;
        let scalar_params = Arc::new(secp256k1_scalar_field_params());
        let base_params = Arc::new(secp256k1_base_field_params());

        let mut seed = Secp256Fr::multiplicative_generator();
        seed = seed.pow([1234]);

        let mut full_table_ids = vec![];
        seq_macro::seq!(C in 0..32 {
            let ids = [
                cs.get_table_id_for_marker::<FixedBaseMulTable<0, C>>()
                    .expect("table must exist"),
                cs.get_table_id_for_marker::<FixedBaseMulTable<1, C>>()
                    .expect("table must exist"),
                cs.get_table_id_for_marker::<FixedBaseMulTable<2, C>>()
                    .expect("table must exist"),
                cs.get_table_id_for_marker::<FixedBaseMulTable<3, C>>()
                    .expect("table must exist"),
                cs.get_table_id_for_marker::<FixedBaseMulTable<4, C>>()
                    .expect("table must exist"),
                cs.get_table_id_for_marker::<FixedBaseMulTable<5, C>>()
                    .expect("table must exist"),
                cs.get_table_id_for_marker::<FixedBaseMulTable<6, C>>()
                    .expect("table must exist"),
                cs.get_table_id_for_marker::<FixedBaseMulTable<7, C>>()
                    .expect("table must exist"),
            ];
            full_table_ids.push(ids);
        });

        for _i in 0..16 {
            let scalar = Secp256ScalarNNField::allocate_checked(cs, seed, &scalar_params);
            let mut result = fixed_base_mul::<GoldilocksField, _, _, _, _, 17>(
                cs,
                scalar,
                &base_params,
                16,
                16,
                &full_table_ids,
            );
            let ((result_x, result_y), _) =
                result.convert_to_affine_or_default(cs, Secp256Affine::one());

            let expected = Secp256Affine::one().mul(seed).into_affine();
            dbg!(_i);
            dbg!(seed);
            assert_eq!(result_x.witness_hook(cs)().unwrap().get(), *expected.as_xy().0);
            assert_eq!(result_y.witness_hook(cs)().unwrap().get(), *expected.as_xy().1);

            seed.square();
        }
    }

    #[test]
    fn test_variable_base_mul() {
        let mut owned_cs = create_cs(1 << 21);
        let cs = &mut owned_cs;
        let scalar_params = Arc::new(secp256k1_scalar_field_params());
        let base_params = Arc::new(secp256k1_base_field_params());

        let mut seed = Secp256Fr::multiplicative_generator();
        seed = seed.pow([1234]);

        let mut seed_2 = Secp256Fr::multiplicative_generator();
        seed_2 = seed_2.pow([987654]);

        for _i in 0..16 {
            dbg!(_i);
            dbg!(seed);

            let base = Secp256Affine::one().mul(seed_2).into_affine();

            // let mut seed = Secp256Fr::from_str("1234567890").unwrap();
            // dbg!(base);
            // dbg!(base.mul(seed).into_affine());

            let scalar = Secp256ScalarNNField::allocate_checked(cs, seed, &scalar_params);
            let x = Secp256BaseNNField::allocate_checked(cs, *base.as_xy().0, &base_params);
            let y = Secp256BaseNNField::allocate_checked(cs, *base.as_xy().1, &base_params);
            let point = SWProjectivePoint::from_xy_unchecked(cs, x, y);

            let mut result =
                width_4_windowed_multiplication(cs, point, scalar, &base_params, &scalar_params);
            let ((result_x, result_y), _) =
                result.convert_to_affine_or_default(cs, Secp256Affine::one());

            let expected = base.mul(seed).into_affine();
            assert_eq!(result_x.witness_hook(cs)().unwrap().get(), *expected.as_xy().0);
            assert_eq!(result_y.witness_hook(cs)().unwrap().get(), *expected.as_xy().1);

            seed.square();
            seed_2.square();
        }
    }

    #[test]
    fn test_signature_for_address_verification() {
        let mut owned_cs = create_cs(1 << 20);
        let cs = &mut owned_cs;

        let sk = crate::ff::from_hex::<Secp256Fr>(
            "b5b1870957d373ef0eeffecc6e4812c0fd08f554b37b233526acc331bf1544f7",
        )
        .unwrap();
        let eth_address = hex::decode("12890d2cce102216644c59dae5baed380d84830c").unwrap();
        let (r, s, _pk, digest) = simulate_signature_for_sk(sk);
        dbg!(_pk);

        let scalar_params = secp256k1_scalar_field_params();
        let base_params = secp256k1_base_field_params();

        let digest_u256 = repr_into_u256(digest.into_repr());
        let r_u256 = repr_into_u256(r.into_repr());
        let s_u256 = repr_into_u256(s.into_repr());

        let rec_id = UInt8::allocate_checked(cs, 0);
        let r = UInt256::allocate(cs, r_u256);
        let s = UInt256::allocate(cs, s_u256);
        let digest = UInt256::allocate(cs, digest_u256);

        let scalar_params = Arc::new(scalar_params);
        let base_params = Arc::new(base_params);

        let valid_x_in_external_field = Secp256BaseNNField::allocated_constant(
            cs,
            Secp256Fq::from_str("9").unwrap(),
            &base_params,
        );
        let valid_t_in_external_field = Secp256BaseNNField::allocated_constant(
            cs,
            Secp256Fq::from_str("16").unwrap(),
            &base_params,
        );
        let valid_y_in_external_field = Secp256BaseNNField::allocated_constant(
            cs,
            Secp256Fq::from_str("4").unwrap(),
            &base_params,
        );

        for _ in 0..5 {
            let (no_error, digest) = ecrecover_precompile_inner_routine::<_, _, true>(
                cs,
                &rec_id,
                &r,
                &s,
                &digest,
                valid_x_in_external_field.clone(),
                valid_y_in_external_field.clone(),
                valid_t_in_external_field.clone(),
                &base_params,
                &scalar_params,
            );

            assert!(no_error.witness_hook(&*cs)().unwrap() == true);
            let recovered_address = digest.to_be_bytes(cs);
            let recovered_address = recovered_address.witness_hook(cs)().unwrap();
            assert_eq!(&recovered_address[12..], &eth_address[..]);
        }

        dbg!(cs.next_available_row());

        cs.pad_and_shrink();

        let mut cs = owned_cs.into_assembly::<std::alloc::Global>();
        cs.print_gate_stats();
        let worker = Worker::new();
        assert!(cs.check_if_satisfied(&worker));
    }

    #[test]
    fn test_signature_from_reference_vector() {
        let mut owned_cs = create_cs(1 << 20);
        let cs = &mut owned_cs;

        let digest =
            hex::decode("38d18acb67d25c8bb9942764b62f18e17054f66a817bd4295423adf9ed98873e")
                .unwrap();
        let v = 0;
        let r = hex::decode("38d18acb67d25c8bb9942764b62f18e17054f66a817bd4295423adf9ed98873e")
            .unwrap();
        let s = hex::decode("789d1dd423d25f0772d2748d60f7e4b81bb14d086eba8e8e8efb6dcff8a4ae02")
            .unwrap();
        let eth_address = hex::decode("ceaccac640adf55b2028469bd36ba501f28b699d").unwrap();

        let scalar_params = secp256k1_scalar_field_params();
        let base_params = secp256k1_base_field_params();

        let digest_u256 = U256::from_big_endian(&digest);
        let r_u256 = U256::from_big_endian(&r);
        let s_u256 = U256::from_big_endian(&s);

        let rec_id = UInt8::allocate_checked(cs, v);
        let r = UInt256::allocate(cs, r_u256);
        let s = UInt256::allocate(cs, s_u256);
        let digest = UInt256::allocate(cs, digest_u256);

        let scalar_params = Arc::new(scalar_params);
        let base_params = Arc::new(base_params);

        let valid_x_in_external_field = Secp256BaseNNField::allocated_constant(
            cs,
            Secp256Fq::from_str("9").unwrap(),
            &base_params,
        );
        let valid_t_in_external_field = Secp256BaseNNField::allocated_constant(
            cs,
            Secp256Fq::from_str("16").unwrap(),
            &base_params,
        );
        let valid_y_in_external_field = Secp256BaseNNField::allocated_constant(
            cs,
            Secp256Fq::from_str("4").unwrap(),
            &base_params,
        );

        for _ in 0..1 {
            let (no_error, digest) = ecrecover_precompile_inner_routine::<_, _, true>(
                cs,
                &rec_id,
                &r,
                &s,
                &digest,
                valid_x_in_external_field.clone(),
                valid_y_in_external_field.clone(),
                valid_t_in_external_field.clone(),
                &base_params,
                &scalar_params,
            );

            assert!(no_error.witness_hook(&*cs)().unwrap() == true);
            let recovered_address = digest.to_be_bytes(cs);
            let recovered_address = recovered_address.witness_hook(cs)().unwrap();
            assert_eq!(&recovered_address[12..], &eth_address[..]);
        }

        dbg!(cs.next_available_row());

        cs.pad_and_shrink();

        let mut cs = owned_cs.into_assembly::<std::alloc::Global>();
        cs.print_gate_stats();
        let worker = Worker::new();
        assert!(cs.check_if_satisfied(&worker));
    }

    #[test]
    fn test_signature_from_reference_vector_2() {
        let mut owned_cs = create_cs(1 << 20);
        let cs = &mut owned_cs;

        let digest =
            hex::decode("14431339128bd25f2c7f93baa611e367472048757f4ad67f6d71a5ca0da550f5")
                .unwrap();
        let v = 1;
        let r = hex::decode("51e4dbbbcebade695a3f0fdf10beb8b5f83fda161e1a3105a14c41168bf3dce0")
            .unwrap();
        let s = hex::decode("46eabf35680328e26ef4579caf8aeb2cf9ece05dbf67a4f3d1f28c7b1d0e3546")
            .unwrap();
        let eth_address = hex::decode("7f8b3b04bf34618f4a1723fba96b5db211279a2b").unwrap();

        let scalar_params = secp256k1_scalar_field_params();
        let base_params = secp256k1_base_field_params();

        let digest_u256 = U256::from_big_endian(&digest);
        let r_u256 = U256::from_big_endian(&r);
        let s_u256 = U256::from_big_endian(&s);

        let rec_id = UInt8::allocate_checked(cs, v);
        let r = UInt256::allocate(cs, r_u256);
        let s = UInt256::allocate(cs, s_u256);
        let digest = UInt256::allocate(cs, digest_u256);

        let scalar_params = Arc::new(scalar_params);
        let base_params = Arc::new(base_params);

        let valid_x_in_external_field = Secp256BaseNNField::allocated_constant(
            cs,
            Secp256Fq::from_str("9").unwrap(),
            &base_params,
        );
        let valid_t_in_external_field = Secp256BaseNNField::allocated_constant(
            cs,
            Secp256Fq::from_str("16").unwrap(),
            &base_params,
        );
        let valid_y_in_external_field = Secp256BaseNNField::allocated_constant(
            cs,
            Secp256Fq::from_str("4").unwrap(),
            &base_params,
        );

        for _ in 0..1 {
            let (no_error, digest) = ecrecover_precompile_inner_routine::<_, _, true>(
                cs,
                &rec_id,
                &r,
                &s,
                &digest,
                valid_x_in_external_field.clone(),
                valid_y_in_external_field.clone(),
                valid_t_in_external_field.clone(),
                &base_params,
                &scalar_params,
            );

            assert!(no_error.witness_hook(&*cs)().unwrap() == true);
            let recovered_address = digest.to_be_bytes(cs);
            let recovered_address = recovered_address.witness_hook(cs)().unwrap();
            assert_eq!(&recovered_address[12..], &eth_address[..]);
        }

        dbg!(cs.next_available_row());

        cs.pad_and_shrink();

        let mut cs = owned_cs.into_assembly::<std::alloc::Global>();
        cs.print_gate_stats();
        let worker = Worker::new();
        assert!(cs.check_if_satisfied(&worker));
    }

    #[test]
    fn test_ecrecover_zero_elements() {
        let mut owned_cs = create_cs(1 << 21);
        let cs = &mut owned_cs;

        let sk = crate::ff::from_hex::<Secp256Fr>(
            "b5b1870957d373ef0eeffecc6e4812c0fd08f554b37b233526acc331bf1544f7",
        )
        .unwrap();
        let (r, s, _pk, digest) = simulate_signature_for_sk(sk);

        let scalar_params = secp256k1_scalar_field_params();
        let base_params = secp256k1_base_field_params();

        let zero_digest = Secp256Fr::zero();
        let zero_r = Secp256Fr::zero();
        let zero_s = Secp256Fr::zero();

        let digest_u256 = repr_into_u256(digest.into_repr());
        let r_u256 = repr_into_u256(r.into_repr());
        let s_u256 = repr_into_u256(s.into_repr());

        let zero_digest_u256 = repr_into_u256(zero_digest.into_repr());
        let zero_r_u256 = repr_into_u256(zero_r.into_repr());
        let zero_s_u256 = repr_into_u256(zero_s.into_repr());

        let rec_id = UInt8::allocate_checked(cs, 0);
        let r = UInt256::allocate(cs, r_u256);
        let s = UInt256::allocate(cs, s_u256);
        let digest = UInt256::allocate(cs, digest_u256);

        let zero_r = UInt256::allocate(cs, zero_r_u256);
        let zero_s = UInt256::allocate(cs, zero_s_u256);
        let zero_digest = UInt256::allocate(cs, zero_digest_u256);

        // Create an r that is unrecoverable.
        let r_unrecoverable =
            UInt256::allocate(cs, U256::from(0u64).overflowing_sub(U256::from(1u64)).0);

        let scalar_params = Arc::new(scalar_params);
        let base_params = Arc::new(base_params);

        let valid_x_in_external_field = Secp256BaseNNField::allocated_constant(
            cs,
            Secp256Fq::from_str("9").unwrap(),
            &base_params,
        );
        let valid_t_in_external_field = Secp256BaseNNField::allocated_constant(
            cs,
            Secp256Fq::from_str("16").unwrap(),
            &base_params,
        );
        let valid_y_in_external_field = Secp256BaseNNField::allocated_constant(
            cs,
            Secp256Fq::from_str("4").unwrap(),
            &base_params,
        );

        // Construct a table of all combinations of correct and incorrect values
        // for r, s, and digest.
        let r_values = vec![r, zero_r, r_unrecoverable];
        let s_values = vec![s, zero_s];
        let digest_values = vec![digest, zero_digest];

        // We ensure that there are no combinations where all correct items are chosen, so that we
        // can consistently check for errors.
        let mut first = true;
        let mut all_combinations = vec![];
        for r in r_values.iter() {
            for s in s_values.iter() {
                for digest in digest_values.iter() {
                    if first {
                        first = false;
                        continue;
                    }
                    all_combinations.push((r.clone(), s.clone(), digest.clone()));
                }
            }
        }

        for (r, s, digest) in all_combinations.into_iter() {
            let (no_error, _digest) = ecrecover_precompile_inner_routine::<_, _, false>(
                cs,
                &rec_id,
                &r,
                &s,
                &digest,
                valid_x_in_external_field.clone(),
                valid_y_in_external_field.clone(),
                valid_t_in_external_field.clone(),
                &base_params,
                &scalar_params,
            );

            assert!(no_error.witness_hook(&*cs)().unwrap() == false);
        }
    }

    // As discussed on ethresearch forums, a caller may 'abuse' ecrecover in order to compute a
    // secp256k1 ecmul in the EVM. This test compares the result of an ecrecover scalar mul with
    // the output of a previously tested ecmul in the EVM.
    //
    // It works as follows: given a point x coordinate `r`, we set `s` to be `r * k` for some `k`.
    // This then works out in the secp256k1 recover equation to create the equation
    // `res = (r, y) * r * k * inv(r, P)` which is equal to `res = (r, y) * k`, effectively
    // performing a scalar multiplication.
    //
    // https://ethresear.ch/t/you-can-kinda-abuse-ecrecover-to-do-ecmul-in-secp256k1-today/2384
    #[test]
    fn test_ecrecover_scalar_mul_trick() {
        let mut owned_cs = create_cs(1 << 20);
        let cs = &mut owned_cs;

        // NOTE: This is essentially reducing a base field to a scalar field element. Due to the
        // nature of the recovery equation turning into `(r, y) * r * k * inv(r, P)`, reducing r to
        // a scalar value would yield the same result regardless.
        let r = crate::ff::from_hex::<Secp256Fr>(
            "00000000000000009b37e91445e92b1423354825aa33d841d83cacfdd895d316ae88dabc31736996",
        )
        .unwrap();
        let k = crate::ff::from_hex::<Secp256Fr>(
            "0000000000000000005aa98b08426f9dea29001fc925f3f35a10c9927082fe4d026cc485d1ebb430",
        )
        .unwrap();
        let mut s = r.clone();
        s.mul_assign(&k);
        let evm_tested_digest = hex::decode("eDc01060fdD6592f54A63EAE6C89436675C4d70D").unwrap();

        let scalar_params = secp256k1_scalar_field_params();
        let base_params = secp256k1_base_field_params();

        let r_u256 = repr_into_u256(r.into_repr());
        let s_u256 = repr_into_u256(s.into_repr());

        let rec_id = UInt8::allocate_checked(cs, 0);
        let r = UInt256::allocate(cs, r_u256);
        let s = UInt256::allocate(cs, s_u256);
        let digest = UInt256::allocate(cs, U256::zero());

        let scalar_params = Arc::new(scalar_params);
        let base_params = Arc::new(base_params);

        let valid_x_in_external_field = Secp256BaseNNField::allocated_constant(
            cs,
            Secp256Fq::from_str("9").unwrap(),
            &base_params,
        );
        let valid_t_in_external_field = Secp256BaseNNField::allocated_constant(
            cs,
            Secp256Fq::from_str("16").unwrap(),
            &base_params,
        );
        let valid_y_in_external_field = Secp256BaseNNField::allocated_constant(
            cs,
            Secp256Fq::from_str("4").unwrap(),
            &base_params,
        );

        for _ in 0..5 {
            let (no_error, digest) = ecrecover_precompile_inner_routine::<_, _, true>(
                cs,
                &rec_id,
                &r,
                &s,
                &digest,
                valid_x_in_external_field.clone(),
                valid_y_in_external_field.clone(),
                valid_t_in_external_field.clone(),
                &base_params,
                &scalar_params,
            );

            // Zero digest shouldn't give us an error
            assert!(no_error.witness_hook(&*cs)().unwrap() == true);
            let recovered_address = digest.to_be_bytes(cs);
            let recovered_address = recovered_address.witness_hook(cs)().unwrap();
            assert_eq!(&recovered_address[12..], &evm_tested_digest[..]);
        }

        dbg!(cs.next_available_row());

        cs.pad_and_shrink();

        let mut cs = owned_cs.into_assembly::<std::alloc::Global>();
        cs.print_gate_stats();
        let worker = Worker::new();
        assert!(cs.check_if_satisfied(&worker));
    }
}
