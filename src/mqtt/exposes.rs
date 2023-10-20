use bitflags::bitflags;
use reusable::{reusable, reuse};
use serde::Deserialize;
use serde_json::Value;

// Based on https://www.zigbee2mqtt.io/guide/usage/exposes.html and https://github.com/Koenkk/zigbee-herdsman-converters/blob/master/src/lib/exposes.ts

#[reusable(ExposeNoProperty)]
#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct ExposeNoProperty {
    /// The name of the exposed value.
    name: String,
    /// A human-readable version of the name.
    label: String,
    // The actions that are available for this value.
    //
    // This is required for all variants except for `Composite`.
    access: Option<Access>,
    /// The type + type specific data.
    #[serde(flatten)]
    variant: ExposeType,
}

#[reuse(ExposeNoProperty)]
#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct Expose {
    /// The property that the exposed value will use in JSON representations.
    property: String,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "lowercase")]
#[allow(dead_code)]
enum ExposeType {
    /// A binary value.
    Binary {
        /// The value to be used for the 'on' state.
        value_on: Value,
        /// The value to be used for the 'off' state.
        value_off: Value,
        /// The value to use to toggle between the 'on' and 'off' states.
        ///
        /// This is only for set actions, returning this from a get would make no sense.
        ///
        /// Using this value with this operator would cause the value to toggle on every reconcile and should not be permitted.
        value_toggle: Option<Value>,
    },

    /// A numeric value.
    Numeric {
        /// The minimum value (inclusive).
        value_min: Option<isize>,
        /// The maximum value (inclusive).
        value_max: Option<isize>,
        /// The step size.
        value_step: Option<isize>,
        /// The human-readable unit to display after the value.
        unit: Option<String>,
        /// A list of values with specific meanings.
        presets: Vec<Preset>,
    },

    /// An enum value.
    Enum {
        /// The allowed values.
        values: Vec<Value>,
    },

    /// A textual value.
    Text {},

    /// A list of values of a single type.
    List {
        /// The mimimum number of items (inclusive).
        length_min: Option<usize>,
        /// The maximum number of items (inclusive).
        length_max: Option<usize>,
        /// The type of the contained item.
        item_type: Box<ExposeNoProperty>,
    },

    /// A nested map.
    #[serde(
        // The specific exposes are all specializations of composite with some constraints on the features, and for our purposes it is not important to distinguish these.
        alias = "light",
        alias = "switch",
        alias = "fan",
        alias = "cover",
        alias = "lock",
        alias = "climate",
    )]
    Composite {
        /// The nested properties.
        features: Vec<Expose>,
    },
}

bitflags! {
    #[derive(Deserialize, Debug)]
    struct Access: u8 {
        /// The property can be found in the published state of this device.
        const PUBLISH = 0b001;
        /// The property can be set with a `/set` command.
        const SET = 0b010;
        /// The property can be retrieved with a `/get` command.
        ///
        /// If this is set the `PUBLISH` flag should also be set.
        const GET = 0b100;
    }
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct Preset {
    name: String,
    description: String,
    value: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_devices() -> Vec<Value> {
        let mut data =
            serde_json::from_str::<Vec<Value>>(include_str!("./exposes-example.json")).unwrap();
        data.remove(0); // first item is source attribution
        data
    }

    #[test]
    fn example_parse() {
        println!("{:?}", load_devices());
    }
}
