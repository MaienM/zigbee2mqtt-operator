//! Subcommand to output the CRD definitions as YAML.

use argh::FromArgs;
use kube::CustomResourceExt;

use crate::crds::{Device, Group, Instance};

/// Output the CRD definitions.
#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "crdgen")]
pub struct Args {}

#[allow(
    missing_docs,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value
)]
pub fn main(_args: Args) {
    let crds = [&Instance::crd(), &Device::crd(), &Group::crd()];
    for crd in crds {
        print!("---\n{}", serde_yaml::to_string(crd).unwrap());
    }
}
