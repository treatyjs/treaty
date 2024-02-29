use swc_common::comments::Comments;
use swc_core::{
    common::FileName,
    ecma::visit::{noop_fold_type, Fold}
};


pub struct TreatyAuthoring;

impl TreatyAuthoring {

}

impl Fold for TreatyAuthoring {
    noop_fold_type!();
    // Implement necessary visit_mut_* methods for actual custom transform.
    // A comprehensive list of possible visitor methods can be found here:
    // https://rustdoc.swc.rs/swc_ecma_visit/trait.VisitMut.html
}


pub fn treaty_authoring<C>(
    _file_name: FileName,
    _src_file_hash: u128,
    _comments: C,
) -> impl Fold
where
    C: Comments,
{

    TreatyAuthoring{}
}
