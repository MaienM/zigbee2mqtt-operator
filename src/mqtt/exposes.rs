//! Check values against the schemas provided in Zigbee2MQTT.
//!
//! Based on <https://www.zigbee2mqtt.io/guide/usage/exposes.html> and <https://github.com/Koenkk/zigbee-herdsman-converters/blob/master/src/lib/exposes.ts>.
//!
//! Note that these even though these links only mention exposed values the same schemas are used for device options.

use bitflags::bitflags;
use derive_builder::Builder;
use enum_dispatch::enum_dispatch;
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::Value;
use structout::generate;

use crate::error::Error;

// Generate structs to hold the data for each type.
//
// This excludes `type` (this will be used as enum discriminator) and `property` (as this isn't present in `List`s, so we'll leave it out so we can use these in that context as well).
generate!(
    #[derive(Builder, Clone, Debug, Default, Deserialize)]
    #[builder(default)]
    #[allow(dead_code)]
    {
        /// The name of the property.
        name: String,
        /// A human-readable version of the name.
        label: Option<String>,
        /// A description of the property.
        description: Option<String>,
        /// The actions that are available for this value.
        access: Access,
        /// The endpoint that the value is exposed on.
        endpoint: Option<String>,
    } => {
        Binary => [
            attr(/** Inner struct for [`Schema::Binary`]. */),
            upsert(
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
            )
        ],
        Numeric => [
            attr(/** Inner struct for [`Schema::Numeric`]. */),
            upsert(
                /// The minimum value (inclusive).
                value_min: Option<f64>,
                /// The maximum value (inclusive).
                value_max: Option<f64>,
                /// The step size.
                value_step: Option<f64>,
                /// The human-readable unit to display after the value.
                unit: Option<String>,
                /// A list of values with specific meanings.
                #[serde(default)]
                presets: Vec<Preset>,
            ),
        ],
        Enum => [
            attr(/** Inner struct for [`Schema::Enum`]. */),
            upsert(
                /// The allowed values.
                values: Vec<Value>,
            ),
        ],
        Text => [
            attr(/** Inner struct for [`Schema::Text`]. */),
        ],
        List => [
            attr(/** Inner struct for [`Schema::List`]. */),
            upsert(
                /// The mimimum number of items (inclusive).
                length_min: Option<usize>,
                /// The maximum number of items (inclusive).
                length_max: Option<usize>,
                /// The type of the contained item.
                item_type: Box<ListItem>,
            ),
        ],
        Bag => [
            attr(/** Inner struct for [`Schema::Bag`]. */),
        ],
        Composite => [
            attr(/** Inner struct for [`Schema::Composite`]. */),
            upsert(
                /// The nested properties.
                features: Vec<Schema>,
                /// The actions that are available for this value.
                access: Option<Access>,
            ),
        ],
        Specific => [
            attr(/** Inner struct for [`Schema::Specific`]. */),
            include(endpoint),
            upsert(
                /// The nested properties.
                features: Vec<Schema>,
            ),
        ],
    }
);

/// A predefined value with a specific meaning.
#[derive(Builder, Clone, Debug, Default, Deserialize)]
#[builder(default)]
#[allow(dead_code)]
struct Preset {
    name: String,
    description: String,
    value: f64,
}

bitflags! {
    #[derive(Clone, Debug, Default, Deserialize)]
    #[serde(from = "u8")]
    /// A bitmask defining what actions can be performed on the value.
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
impl From<u8> for Access {
    fn from(value: u8) -> Self {
        Access::from_bits_truncate(value)
    }
}

/// Wrapper for a schema struct that is attached to specific property of an object.
#[derive(Builder, Clone, Debug, Deserialize)]
#[allow(dead_code)]
struct WithProperty<T>
where
    T: Clone,
{
    property: String,
    #[serde(flatten)]
    type_: T,
}

/// Definition of a value that is exposed by a Zigbee2MQTT device.
///
/// The primary moniker used for this in Zigbee2MQTT is `exposes`, when nested they're called `features` instead. We'll use schema in this module, in part because in addition to exposed values they're also used for device options.
#[derive(Clone, Debug, Deserialize)]
#[enum_dispatch(PropertyHolder, Processor)]
#[serde(tag = "type", rename_all = "lowercase")]
#[allow(dead_code)]
enum Schema {
    /// A binary value.
    Binary(WithProperty<Binary>),
    /// A numeric value.
    Numeric(WithProperty<Numeric>),
    /// An enum value.
    Enum(WithProperty<Enum>),
    /// A textual value.
    Text(WithProperty<Text>),
    /// A list of values of a single type.
    List(WithProperty<List>),
    /// A nested map with predefined keys/properties.
    Composite(WithProperty<Composite>),
    /// Specific types.
    ///
    /// These define a list of related properties. Each type has some restrictions on what fields it must/can contain, but since we're just consuming the schema this is of little consequence to us.
    ///
    /// These appear similar to composite, but unlike composite these _don't_ define a nested object. Instead all contained features exist on the same level as their parent. Because of this they lack most attributes that the rest of the schema types have as these are defined on the contained features.
    #[serde(
        rename = "light",
        alias = "switch",
        alias = "fan",
        alias = "cover",
        alias = "lock",
        alias = "climate"
    )]
    Specific(Specific),
    /// An object with undefined contents.
    ///
    /// This exists purely for the `homeassistant` option, as we don't have a definition for this, so we have no options option than to just accept any arbitrary object.
    #[serde(skip)]
    Bag(WithProperty<Bag>),
}

/// The value types that can be used in a list.
///
/// This is `Schema` with the following changes:
/// - remove the `property` key as list items cannot define their own property, they exist in the property that the list is exposed as
/// - exclude specific types as the features within it would end up directly in the list which result in the same issue.
#[derive(Clone, Debug, Deserialize)]
#[enum_dispatch(Processor)]
#[serde(tag = "type", rename_all = "lowercase")]
#[allow(dead_code)]
enum ListItem {
    Binary(Binary),
    Numeric(Numeric),
    Enum(Enum),
    Text(Text),
    List(List),
    Composite(Composite),
}
impl Default for ListItem {
    fn default() -> Self {
        Self::Text(Text::default())
    }
}

/// Struct to build a [`Error::InvalidResource`].
#[derive(Debug, Eq, PartialEq)]
pub struct InvalidValue {
    /// The path of the value relative to the start.
    ///
    /// This is a partial JSON path that is intended to be appended directly to the key of the start value (e.g. `.foo` or `[12]`).
    path: String,
    /// A human-readable message describing the problem with the value.
    message: String,
}
impl InvalidValue {
    fn new(message: &str) -> Self {
        Self {
            path: String::new(),
            message: message.to_owned(),
        }
    }
    fn prefix_path(mut self, prefix: String) -> Self {
        self.path = prefix + &self.path;
        self
    }
}
impl From<InvalidValue> for Error {
    fn from(value: InvalidValue) -> Self {
        Error::InvalidResource {
            field_path: value.path,
            message: value.message,
        }
    }
}

/// Convert a [`Value`] to string.
macro_rules! to_json {
    ($val:expr) => {
        serde_json::to_string(&$val).unwrap()
    };
}

/// Trait to check whether a property is defined.
#[enum_dispatch]
trait PropertyHolder {
    /// Check whether a property with the given name is known to this part of the schema.
    fn knows_property(&self, name: &str) -> bool;
}
impl PropertyHolder for Specific {
    fn knows_property(&self, name: &str) -> bool {
        self.features.knows_property(name)
    }
}
impl<T> PropertyHolder for WithProperty<T>
where
    T: Clone,
{
    fn knows_property(&self, name: &str) -> bool {
        self.property == name
    }
}
impl PropertyHolder for Vec<Schema> {
    fn knows_property(&self, name: &str) -> bool {
        self.iter().any(|f| f.knows_property(name))
    }
}

/// Trait to process a value
#[enum_dispatch]
pub trait Processor {
    /// Process the value, validating it matches the schema, and transforming it as needed.
    ///
    /// The caller _must_ use the returned value (which may be different from the input).
    fn process(&self, value: Value) -> Result<Value, InvalidValue>;
}
impl Processor for Binary {
    fn process(&self, value: Value) -> Result<Value, InvalidValue> {
        if value == self.value_on || value == self.value_off {
            Ok(value)
        } else if Some(value) == self.value_toggle {
            Err(InvalidValue::new(&format!(
                "This will toggle the state on every reconcile. Use {value_on} or {value_off} instead.",
                value_on = to_json!(self.value_on),
                value_off = to_json!(self.value_off),
            )))
        } else {
            Err(InvalidValue::new(&format!(
                "Must be either {value_on} or {value_off}.",
                value_on = to_json!(self.value_on),
                value_off = to_json!(self.value_off),
            )))
        }
    }
}
impl Processor for Numeric {
    fn process(&self, value: Value) -> Result<Value, InvalidValue> {
        // Instead of a numeric value the name of a preset can be used. Check this first so we can treat it as a number after thsi point.
        if let Some(preset_name) = value.as_str() {
            for preset in &self.presets {
                if preset_name == preset.name {
                    return Ok(preset.value.into());
                }
            }
        }

        let Some(num) = value.as_f64() else {
            let preset_names = self.presets.iter().map(|p| &p.name).collect::<Vec<_>>();
            return if preset_names.is_empty() {
                Err(InvalidValue::new("Must be a number."))
            } else {
                Err(InvalidValue::new(&format!(
                    "Must be a number or one of the presets: {preset_names}.",
                    preset_names = to_json!(preset_names)
                )))
            };
        };

        match (self.value_min, self.value_max) {
            (Some(min), Some(max)) => {
                if num < min || num > max {
                    return Err(InvalidValue::new(&format!(
                        "Must be between {min} and {max} (inclusive)."
                    )));
                }
            }
            (Some(min), None) => {
                if num < min {
                    return Err(InvalidValue::new(&format!("Must be at least {min}.")));
                }
            }
            (None, Some(max)) => {
                if num > max {
                    return Err(InvalidValue::new(&format!("Must be at most {max}.")));
                }
            }
            (None, None) => {}
        };
        if let Some(step) = self.value_step {
            // This is assuming that steps should be taken from the min value, so a range of 11-31 with steps of 2 would accept 11, 13, 15, etc.
            let start = self.value_min.unwrap_or(0.0);
            if (num - start) % step != 0.0 {
                return Err(InvalidValue::new(&format!(
                    "Must be {start} plus any number of steps of {step} (e.g. {example_valid_1}, {example_valid_2}, but not {example_invalid}).",
                    example_valid_1 = start + step,
                    example_valid_2 = start + step * 2.0,
                    example_invalid = start + step * 1.5,
                )));
            }
        }
        Ok(value.clone())
    }
}
impl Processor for Enum {
    fn process(&self, value: Value) -> Result<Value, InvalidValue> {
        if self.values.contains(&value) {
            Ok(value.clone())
        } else {
            Err(InvalidValue::new(&format!(
                "Must be one of {values}.",
                values = to_json!(self.values),
            )))
        }
    }
}
impl Processor for Text {
    fn process(&self, value: Value) -> Result<Value, InvalidValue> {
        if value.is_string() {
            Ok(value.clone())
        } else {
            Err(InvalidValue::new("Must be a string."))
        }
    }
}
impl Processor for List {
    fn process(&self, value: Value) -> Result<Value, InvalidValue> {
        let Some(items) = value.as_array() else {
            return Err(InvalidValue::new("Must be an array."));
        };

        let len = items.len();
        match (self.length_min, self.length_max) {
            (Some(min), Some(max)) => {
                if len < min || len > max {
                    return Err(InvalidValue::new(&format!(
                        "Must have between {min} and {max} items (inclusive)."
                    )));
                }
            }
            (Some(min), None) => {
                if len < min {
                    return Err(InvalidValue::new(&format!(
                        "Must have at least {min} item(s)."
                    )));
                }
            }
            (None, Some(max)) => {
                if len > max {
                    return Err(InvalidValue::new(&format!(
                        "Must have at most {max} item(s)."
                    )));
                }
            }
            (None, None) => {}
        };

        let mut new_items = Vec::new();
        for (idx, item) in items.iter().cloned().enumerate() {
            new_items.push(
                self.item_type
                    .process(item)
                    .map_err(|err| err.prefix_path(format!("[{idx}]")))?,
            );
        }
        Ok(Value::Array(new_items))
    }
}
impl Processor for Composite {
    fn process(&self, value: Value) -> Result<Value, InvalidValue> {
        let Some(object) = value.as_object() else {
            return Err(InvalidValue::new("Must be an object."));
        };
        for name in object.keys() {
            if !self.features.knows_property(name) {
                return Err(InvalidValue {
                    path: format!(".{name}"),
                    message: "Unknown property.".to_owned(),
                });
            }
        }

        self.features.process(value)
    }
}
impl Processor for Specific {
    fn process(&self, value: Value) -> Result<Value, InvalidValue> {
        self.features.process(value)
    }
}
impl Processor for Bag {
    fn process(&self, value: Value) -> Result<Value, InvalidValue> {
        if !value.is_object() {
            return Err(InvalidValue::new("Must be an object."));
        };
        Ok(value)
    }
}
impl<T> Processor for WithProperty<T>
where
    T: Clone + Processor,
{
    fn process(&self, mut value: Value) -> Result<Value, InvalidValue> {
        let Some(object) = value.as_object_mut() else {
            return Err(InvalidValue::new("Must be an object."));
        };

        let Some(item) = object.remove(&self.property) else {
            return Ok(value);
        };

        let result = match item {
            Value::Null => {
                // Null value indicates unset/reset, so this is always valid.
                Value::Null
            }
            item => self
                .type_
                .process(item)
                .map_err(|err| err.prefix_path(format!(".{property}", property = self.property)))?,
        };
        object.insert(self.property.clone(), result);

        Ok(value)
    }
}
impl Processor for Vec<Schema> {
    fn process(&self, value: Value) -> Result<Value, InvalidValue> {
        let mut value = value.clone();
        for feature in self {
            value = feature.process(value)?;
        }
        Ok(value)
    }
}

/// The [common device options](https://www.zigbee2mqtt.io/guide/configuration/devices-groups.html#common-device-options).
static COMMON_DEVICE_OPTIONS: Lazy<Vec<Schema>> = Lazy::new(|| {
    let type_bool = BinaryBuilder::default()
        .value_on(true.into())
        .value_off(false.into())
        .build()
        .unwrap();
    let type_stringlist = ListBuilder::default()
        .item_type(Box::new(ListItem::Text(Text::default())))
        .build()
        .unwrap();
    vec![
        Schema::Text(WithProperty {
            property: "description".to_owned(),
            type_: Text::default(),
        }),
        Schema::Binary(WithProperty {
            property: "retain".to_owned(),
            type_: type_bool.clone(),
        }),
        Schema::Binary(WithProperty {
            property: "disabled".to_owned(),
            type_: type_bool.clone(),
        }),
        Schema::Numeric(WithProperty {
            property: "retention".to_owned(),
            type_: NumericBuilder::default()
                .value_min(Some(1.into()))
                .value_step(Some(1.into()))
                .build()
                .unwrap(),
        }),
        Schema::Numeric(WithProperty {
            property: "qos".to_owned(),
            type_: NumericBuilder::default()
                .value_min(Some(0.into()))
                .value_max(Some(2.into()))
                .value_step(Some(1.into()))
                .build()
                .unwrap(),
        }),
        Schema::Bag(WithProperty {
            property: "homeassistant".to_owned(),
            type_: Bag::default(),
        }),
        Schema::Numeric(WithProperty {
            property: "debounce".to_owned(),
            type_: NumericBuilder::default()
                .value_min(Some(1.into()))
                .build()
                .unwrap(),
        }),
        Schema::List(WithProperty {
            property: "debounce_ignore".to_owned(),
            type_: type_stringlist.clone(),
        }),
        Schema::List(WithProperty {
            property: "filtered_attributes".to_owned(),
            type_: type_stringlist.clone(),
        }),
        Schema::List(WithProperty {
            property: "filtered_cache".to_owned(),
            type_: type_stringlist.clone(),
        }),
        Schema::Binary(WithProperty {
            property: "optimistic".to_owned(),
            type_: type_bool.clone(),
        }),
        Schema::List(WithProperty {
            property: "filtered_optimistic".to_owned(),
            type_: type_stringlist.clone(),
        }),
    ]
});

/// Top-level schema.
macro_rules! create_toplevel {
    (
        $(#[$meta:meta])*
        $name:ident, $extra:expr $(,)?
    ) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Default, Deserialize)]
        #[serde(from = "Vec<Schema>")]
        pub struct $name(Composite);
        impl From<Vec<Schema>> for $name {
            fn from(mut value: Vec<Schema>) -> Self {
                value.append($extra);
                Self(Composite {
                    features: value,
                    ..Composite::default()
                })
            }
        }
        impl Processor for $name {
            fn process(&self, value: Value) -> Result<Value, InvalidValue> {
                self.0.process(value)
            }
        }
    };
}
create_toplevel!(
    /// Schema for device options.
    DeviceOptionsSchema,
    &mut COMMON_DEVICE_OPTIONS.clone(),
);
create_toplevel!(
    /// Schema for device capabilities.
    DeviceCapabilitiesSchema,
    &mut vec![]
);

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[derive(Deserialize, Debug)]
    #[allow(dead_code)]
    struct Device {
        definition: Option<Definition>,
    }
    #[derive(Deserialize, Debug)]
    #[allow(dead_code)]
    struct Definition {
        exposes: DeviceCapabilitiesSchema,
        options: DeviceOptionsSchema,
    }

    #[test]
    fn parse_examples() {
        serde_yaml::from_str::<Vec<Device>>(include_str!("./exposes-examples.yaml")).unwrap();
    }

    macro_rules! assert_roundtrip {
        ($expose:ident, $value:expr $(,)?) => {
            let value: Value = $value;
            assert_eq!($expose.process(value.clone()), Ok(value));
        };
    }

    mod process_binary {
        use pretty_assertions::assert_eq;

        use super::*;

        static EXPOSE: Lazy<Binary> = Lazy::new(|| {
            BinaryBuilder::default()
                .value_on("ON".into())
                .value_off("OFF".into())
                .value_toggle(Some("TOGGLE".into()))
                .build()
                .unwrap()
        });

        #[test]
        fn accept_on_off() {
            assert_roundtrip!(EXPOSE, "ON".into());
            assert_roundtrip!(EXPOSE, "OFF".into());
        }

        #[test]
        fn reject_toggle() {
            assert_eq!(
                EXPOSE.process("TOGGLE".into()),
                Err(InvalidValue::new(
                    r#"This will toggle the state on every reconcile. Use "ON" or "OFF" instead."#
                ))
            );
        }

        #[test]
        fn reject_other() {
            let err = Err(InvalidValue::new(r#"Must be either "ON" or "OFF"."#));
            assert_eq!(EXPOSE.process("INVALID".into()), err);
            assert_eq!(EXPOSE.process(10.into()), err);
            assert_eq!(EXPOSE.process(true.into()), err);
            assert_eq!(EXPOSE.process(vec!["ON"].into()), err);
        }
    }

    mod process_numeric {
        use pretty_assertions::assert_eq;

        use super::*;

        #[test]
        fn unrestricted() {
            let expose = Numeric::default();
            assert_roundtrip!(expose, 0.into());
            assert_roundtrip!(expose, 50.into());

            let type_err = Err(InvalidValue::new("Must be a number."));
            assert_eq!(expose.process("50".into()), type_err);
            assert_eq!(expose.process(true.into()), type_err);
            assert_eq!(expose.process(vec![10].into()), type_err);
        }

        #[test]
        fn min() {
            let expose = NumericBuilder::default()
                .value_min(Some(10.0))
                .build()
                .unwrap();
            assert_eq!(
                expose.process(0.into()),
                Err(InvalidValue::new("Must be at least 10."))
            );
            assert_roundtrip!(expose, 50.into());
        }

        #[test]
        fn max() {
            let expose = NumericBuilder::default()
                .value_max(Some(10.0))
                .build()
                .unwrap();
            assert_roundtrip!(expose, 0.into());
            assert_eq!(
                expose.process(50.into()),
                Err(InvalidValue::new("Must be at most 10."))
            );
        }

        #[test]
        fn min_max() {
            let expose = NumericBuilder::default()
                .value_min(Some(10.0))
                .value_max(Some(100.0))
                .build()
                .unwrap();
            assert_eq!(
                expose.process(0.into()),
                Err(InvalidValue::new("Must be between 10 and 100 (inclusive)."))
            );
            assert_roundtrip!(expose, 50.into());
        }

        #[test]
        fn step() {
            let expose = NumericBuilder::default()
                .value_min(Some(1.0))
                .value_step(Some(2.0))
                .build()
                .unwrap();
            assert_eq!(
                expose.process(0.into()),
                Err(InvalidValue::new("Must be at least 1."))
            );
            assert_eq!(
                expose.process(2.into()),
                Err(InvalidValue::new(
                    "Must be 1 plus any number of steps of 2 (e.g. 3, 5, but not 4)."
                ))
            );
            assert_roundtrip!(expose, 13.into());
        }

        #[test]
        fn presets() {
            let expose = NumericBuilder::default()
                .presets(vec![
                    PresetBuilder::default()
                        .name("default".to_owned())
                        .value(25.0)
                        .build()
                        .unwrap(),
                    PresetBuilder::default()
                        .name("previous".to_owned())
                        .value(-1.0)
                        .build()
                        .unwrap(),
                ])
                .build()
                .unwrap();
            assert_roundtrip!(expose, 0.into());
            assert_roundtrip!(expose, 25.into());
            assert_eq!(expose.process("default".into()), Ok((25.0).into()));
            assert_eq!(expose.process("previous".into()), Ok((-1.0).into()));
            assert_eq!(
                expose.process("invalid".into()),
                Err(InvalidValue::new(
                    r#"Must be a number or one of the presets: ["default","previous"]."#
                ))
            );
            assert_eq!(
                expose.process(true.into()),
                Err(InvalidValue::new(
                    r#"Must be a number or one of the presets: ["default","previous"]."#
                ))
            );
        }
    }

    #[test]
    fn process_enum() {
        let expose = EnumBuilder::default()
            .values(vec!["RED".into(), "GREEN".into(), "BLUE".into()])
            .build()
            .unwrap();
        assert_roundtrip!(expose, "RED".into());

        let err = Err(InvalidValue::new(
            r#"Must be one of ["RED","GREEN","BLUE"]."#,
        ));
        assert_eq!(expose.process("CYAN".into()), err);
        assert_eq!(expose.process(10.into()), err);
        assert_eq!(expose.process(true.into()), err);
    }

    #[test]
    fn process_text() {
        let expose = Text::default();

        assert_roundtrip!(expose, "HELLO".into());

        let type_err = Err(InvalidValue::new("Must be a string."));
        assert_eq!(expose.process(10.into()), type_err);
        assert_eq!(expose.process(true.into()), type_err);
    }

    mod process_list {
        use pretty_assertions::assert_eq;

        use super::*;

        #[test]
        fn string() {
            let expose = List::default();
            assert_roundtrip!(expose, vec!["HELLO", "WORLD"].into());

            let err = Err(InvalidValue {
                message: "Must be a string.".to_owned(),
                path: "[1]".to_owned(),
            });
            assert_eq!(
                expose.process(Value::Array(vec!["HELLO".into(), 10.into()])),
                err,
            );
            assert_eq!(
                expose.process(Value::Array(vec!["HELLO".into(), Value::Null])),
                err,
            );
        }

        #[test]
        fn numeric() {
            let expose = ListBuilder::default()
                .item_type(Box::new(ListItem::Numeric(Numeric::default())))
                .build()
                .unwrap();
            assert_roundtrip!(expose, vec![10, 20].into());

            let err = Err(InvalidValue {
                message: "Must be a number.".to_owned(),
                path: "[0]".to_owned(),
            });
            assert_eq!(
                expose.process(Value::Array(vec!["HELLO".into(), 10.into()])),
                err,
            );
            assert_eq!(
                expose.process(Value::Array(vec![Value::Null, 10.into()])),
                err,
            );
        }

        #[test]
        fn min() {
            let expose = ListBuilder::default()
                .length_min(Some(1))
                .item_type(Box::new(ListItem::Numeric(Numeric::default())))
                .build()
                .unwrap();
            assert_roundtrip!(expose, vec![1].into());
            assert_roundtrip!(expose, vec![1, 2, 3].into());
            assert_eq!(
                expose.process(Vec::<f64>::new().into()),
                Err(InvalidValue::new("Must have at least 1 item(s).")),
            );
        }

        #[test]
        fn max() {
            let expose = ListBuilder::default()
                .length_max(Some(2))
                .item_type(Box::new(ListItem::Numeric(Numeric::default())))
                .build()
                .unwrap();
            assert_roundtrip!(expose, vec![1].into());
            assert_roundtrip!(expose, vec![1, 2].into());
            assert_eq!(
                expose.process(vec![1, 2, 3].into()),
                Err(InvalidValue::new("Must have at most 2 item(s).")),
            );
        }

        #[test]
        fn min_max() {
            let expose = ListBuilder::default()
                .length_min(Some(1))
                .length_max(Some(3))
                .item_type(Box::new(ListItem::Numeric(Numeric::default())))
                .build()
                .unwrap();
            assert_roundtrip!(expose, vec![1].into());
            assert_roundtrip!(expose, vec![1, 2].into());
            assert_eq!(
                expose.process(Vec::<f64>::new().into()),
                Err(InvalidValue::new(
                    "Must have between 1 and 3 items (inclusive)."
                )),
            );
            assert_eq!(
                expose.process(vec![1, 2, 3, 4].into()),
                Err(InvalidValue::new(
                    "Must have between 1 and 3 items (inclusive)."
                )),
            );
        }
    }

    #[test]
    fn process_bag() {
        let expose = Bag::default();

        assert_roundtrip!(
            expose,
            json!({
                "foo": 1,
                "bar": 2,
            })
        );

        let type_err = Err(InvalidValue::new("Must be an object."));
        assert_eq!(expose.process(10.into()), type_err);
        assert_eq!(expose.process(true.into()), type_err);
    }

    mod process_capabilities {
        use pretty_assertions::assert_eq;

        use super::*;

        static EXPOSE: Lazy<DeviceCapabilitiesSchema> = Lazy::new(|| {
            DeviceCapabilitiesSchema::from(vec![
                Schema::Numeric(WithProperty {
                    property: "num".to_owned(),
                    type_: NumericBuilder::default()
                        .presets(vec![PresetBuilder::default()
                            .name("default".to_owned())
                            .value(25.0)
                            .build()
                            .unwrap()])
                        .build()
                        .unwrap(),
                }),
                Schema::Text(WithProperty {
                    property: "text".to_owned(),
                    type_: Text::default(),
                }),
                Schema::Composite(WithProperty {
                    property: "nested".to_owned(),
                    type_: CompositeBuilder::default()
                        .features(vec![Schema::Binary(WithProperty {
                            property: "binary".to_owned(),
                            type_: BinaryBuilder::default()
                                .value_on(true.into())
                                .value_off(false.into())
                                .build()
                                .unwrap(),
                        })])
                        .build()
                        .unwrap(),
                }),
                Schema::Specific(
                    SpecificBuilder::default()
                        .features(vec![Schema::Enum(WithProperty {
                            property: "enum".to_owned(),
                            type_: EnumBuilder::default()
                                .values(vec!["RED".into(), "GREEN".into(), "BLUE".into()])
                                .build()
                                .unwrap(),
                        })])
                        .build()
                        .unwrap(),
                ),
                Schema::Bag(WithProperty {
                    property: "bag".to_owned(),
                    type_: Bag::default(),
                }),
            ])
        });

        #[test]
        fn basic() {
            assert_roundtrip!(
                EXPOSE,
                json!({
                    "num": 15,
                    "text": "HELLO",
                    "nested": {
                        "binary": false,
                    },
                    "enum": "BLUE",
                }),
            );

            let type_err = Err(InvalidValue::new("Must be an object."));
            assert_eq!(EXPOSE.process(10.into()), type_err);
            assert_eq!(EXPOSE.process(true.into()), type_err);
            assert_eq!(EXPOSE.process("HELLO".into()), type_err);
        }

        #[test]
        fn error_path_toplevel() {
            assert_eq!(
                EXPOSE.process(json!({
                    "num": "five",
                })),
                Err(InvalidValue {
                    path: ".num".to_owned(),
                    message: r#"Must be a number or one of the presets: ["default"]."#.to_owned(),
                }),
            );
        }

        #[test]
        fn error_path_nested_capability() {
            assert_eq!(
                EXPOSE.process(json!({
                    "nested": {
                        "binary": "maybe",
                    },
                })),
                Err(InvalidValue {
                    path: ".nested.binary".to_owned(),
                    message: "Must be either true or false.".to_owned(),
                }),
            );
        }

        #[test]
        fn error_path_nested_specific() {
            assert_eq!(
                EXPOSE.process(json!({
                    "enum": "CYAN",
                })),
                Err(InvalidValue {
                    path: ".enum".to_owned(),
                    message: r#"Must be one of ["RED","GREEN","BLUE"]."#.to_owned(),
                }),
            );
        }

        #[test]
        fn transform() {
            assert_eq!(
                EXPOSE.process(json!({
                    "text": "HELLO",
                    "num": "default",
                    "enum": "BLUE",
                })),
                Ok(json!({
                    "text": "HELLO",
                    "num": 25.0,
                    "enum": "BLUE",
                })),
            );
        }

        #[test]
        fn null() {
            // Null value indicatee a field should be reset to default.
            assert_roundtrip!(
                EXPOSE,
                json!({
                    "num": null,
                }),
            );
        }

        #[test]
        fn unknown_toplevel() {
            assert_eq!(
                EXPOSE.process(json!({
                    "unknown": 10,
                })),
                Err(InvalidValue {
                    path: ".unknown".to_owned(),
                    message: "Unknown property.".to_owned(),
                }),
            );
        }

        #[test]
        fn unknown_nested() {
            assert_eq!(
                EXPOSE.process(json!({
                    "nested": {
                        "unknown": 10,
                    },
                })),
                Err(InvalidValue {
                    path: ".nested.unknown".to_owned(),
                    message: "Unknown property.".to_owned(),
                }),
            );
        }

        #[test]
        fn unknown_nested_bag() {
            assert_roundtrip!(
                EXPOSE,
                json!({
                    "bag": {
                        "foo": 10,
                    },
                })
            );
        }

        #[test]
        fn device_options_common() {
            let expose = DeviceOptionsSchema::from(vec![]);

            assert_roundtrip!(
                expose,
                json!({
                    "description": "Example",
                    "retain": true,
                })
            );
            assert_eq!(
                expose.process(json!({
                    "unknown": 10,
                })),
                Err(InvalidValue {
                    path: ".unknown".to_owned(),
                    message: "Unknown property.".to_owned(),
                })
            );
        }
    }
}
