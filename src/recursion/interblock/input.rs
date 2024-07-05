use std::collections::VecDeque;

use boojum::{
    cs::implementations::proof::Proof,
    field::{FieldExtension, SmallField},
    gadgets::{
        num::Num, recursion::recursive_tree_hasher::RecursiveTreeHasher,
        traits::allocatable::CSAllocatable,
    },
};

use super::*;

#[derive(Derivative, serde::Serialize, serde::Deserialize)]
#[derivative(Clone, Debug, Default(bound = ""))]
#[serde(
    bound = "<H::CircuitOutput as CSAllocatable<F>>::Witness: serde::Serialize + serde::de::DeserializeOwned"
)]
pub struct InterblockRecursionCircuitInstanceWitness<
    F: SmallField,
    H: RecursiveTreeHasher<F, Num<F>>,
    EXT: FieldExtension<2, BaseField = F>,
> {
    #[derivative(Debug = "ignore")]
    pub proof_witnesses: VecDeque<Proof<F, H::NonCircuitSimulator, EXT>>,
}
