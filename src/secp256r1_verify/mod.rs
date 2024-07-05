use boojum::{
    cs::traits::cs::ConstraintSystem,
    field::SmallField,
    gadgets::{
        boolean::Boolean,
        non_native_field::implementations::*,
        queue::QueueState,
        traits::{selectable::Selectable, witnessable::WitnessHookable},
    },
};
use cs_derive::*;

use super::*;
use crate::{
    base_structures::{log_query::*, memory_query::*},
    ethereum_types::U256,
    fsm_input_output::*,
};

pub mod input;
pub use self::input::*;

pub mod fixed_base_mul_table;
pub mod secp256r1;

pub use self::fixed_base_mul_table::*;

pub const MEMORY_QUERIES_PER_CALL: usize = 5;

pub mod baseline;

// characteristics of the base field for secp curve
use self::secp256r1::fq::Fq as Secp256Fq;
// order of group of points for secp curve
use self::secp256r1::fr::Fr as Secp256Fr;
// some affine point
use self::secp256r1::PointAffine as Secp256Affine;

const BASE_FIELD_REPR_LIMBS: usize = 17;
const SCALAR_FIELD_REPR_LIMBS: usize = 17;
const BASE_FIELD_CANONICAL_REPR_LIMBS: usize = 16;
const SCALAR_FIELD_CANONICAL_REPR_LIMBS: usize = 16;

type Secp256BaseNNFieldParams = NonNativeFieldOverU16Params<Secp256Fq, 17>;
type Secp256ScalarNNFieldParams = NonNativeFieldOverU16Params<Secp256Fr, 17>;

type Secp256BaseNNField<F> = NonNativeFieldOverU16<F, Secp256Fq, 17>;
type Secp256ScalarNNField<F> = NonNativeFieldOverU16<F, Secp256Fr, 17>;

fn secp256r1_base_field_params() -> Secp256BaseNNFieldParams {
    NonNativeFieldOverU16Params::create()
}

fn secp256r1_scalar_field_params() -> Secp256ScalarNNFieldParams {
    NonNativeFieldOverU16Params::create()
}

// re-exports for integration
pub use self::baseline::{
    secp256r1_verify_function_entry_point, Secp256r1VerifyPrecompileCallParams,
};
