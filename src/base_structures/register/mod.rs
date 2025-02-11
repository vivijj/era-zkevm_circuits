use boojum::{
    cs::{
        traits::cs::{ConstraintSystem, DstBuffer},
        Variable,
    },
    field::SmallField,
    gadgets::{
        boolean::Boolean,
        traits::{
            allocatable::{CSAllocatable, CSAllocatableExt},
            encodable::CircuitVarLengthEncodable,
            selectable::Selectable,
            witnessable::WitnessHookable,
        },
        u16::UInt16,
        u256::UInt256,
        u32::UInt32,
    },
};
use cs_derive::*;

use super::*;

#[derive(Derivative, CSSelectable, CSAllocatable, CSVarLengthEncodable, WitnessHookable)]
#[derivative(Clone, Copy, Debug, Hash)]
pub struct VMRegister<F: SmallField> {
    pub is_pointer: Boolean<F>,
    pub value: UInt256<F>,
}

impl<F: SmallField> VMRegister<F> {
    pub fn zero<CS: ConstraintSystem<F>>(cs: &mut CS) -> Self {
        let boolean_false = Boolean::allocated_constant(cs, false);
        let zero_u256 = UInt256::zero(cs);

        Self { is_pointer: boolean_false, value: zero_u256 }
    }

    pub fn from_imm<CS: ConstraintSystem<F>>(cs: &mut CS, imm: UInt16<F>) -> Self {
        let boolean_false = Boolean::allocated_constant(cs, false);
        let zero_u32 = UInt32::zero(cs);

        Self {
            is_pointer: boolean_false,
            value: UInt256 {
                inner: [
                    unsafe { UInt32::from_variable_unchecked(imm.get_variable()) },
                    zero_u32,
                    zero_u32,
                    zero_u32,
                    zero_u32,
                    zero_u32,
                    zero_u32,
                    zero_u32,
                ],
            },
        }
    }

    pub fn conditionally_erase<CS: ConstraintSystem<F>>(
        &mut self,
        cs: &mut CS,
        condition: Boolean<F>,
    ) {
        self.is_pointer = self.is_pointer.mask_negated(cs, condition);
        self.value = self.value.mask_negated(cs, condition);
    }

    pub fn conditionally_erase_fat_pointer_data<CS: ConstraintSystem<F>>(
        &mut self,
        cs: &mut CS,
        condition: Boolean<F>,
    ) {
        self.is_pointer = self.is_pointer.mask_negated(cs, condition);
        // we need to erase bits 32-64 and 64-96
        self.value.inner[1] = self.value.inner[1].mask_negated(cs, condition);
        self.value.inner[2] = self.value.inner[2].mask_negated(cs, condition);
    }
}

impl<F: SmallField> CSAllocatableExt<F> for VMRegister<F> {
    const INTERNAL_STRUCT_LEN: usize = 9;

    fn flatten_as_variables(&self) -> [Variable; Self::INTERNAL_STRUCT_LEN] {
        // NOTE: CSAllocatable is done by the macro, so it allocates in the order of declaration,
        // and we should do the same here!

        [
            self.is_pointer.get_variable(),
            self.value.inner[0].get_variable(),
            self.value.inner[1].get_variable(),
            self.value.inner[2].get_variable(),
            self.value.inner[3].get_variable(),
            self.value.inner[4].get_variable(),
            self.value.inner[5].get_variable(),
            self.value.inner[6].get_variable(),
            self.value.inner[7].get_variable(),
        ]
    }

    fn set_internal_variables_values(_witness: Self::Witness, _dst: &mut DstBuffer<'_, '_, F>) {
        todo!()
    }

    fn witness_from_set_of_values(_values: [F; Self::INTERNAL_STRUCT_LEN]) -> Self::Witness {
        todo!()
    }
}
