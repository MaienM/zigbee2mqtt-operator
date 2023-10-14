use kube::CustomResourceExt;
use zigbee2mqtt_operator::crds::*;

fn main() {
    let crds = [&Instance::crd()];
    for crd in crds {
        print!("---\n{}", serde_yaml::to_string(crd).unwrap());
    }
}
