//! Helpers to track the source of a value.

use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
    hash::Hash,
    ops::{Deref, DerefMut},
};

use serde::Serialize;
use serde_json::{Map, Value};

/// A value with information about its source.
///
/// This attempts to act as its contained value as much as possible, completely disregarding the source for many implemented traits like Display, Hash, etc.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct ValueWithSource<T> {
    value: T,

    /// The source for the value. This must be a valid [`ObjectReference::field_path`] for the resource that is currently being reconciled.
    source: Option<String>,
}
impl<T> ValueWithSource<T> {
    /// Create a VWS with the same source as this one.
    pub fn with_value<V>(&self, value: V) -> ValueWithSource<V> {
        self.sub(value, "")
    }

    /// Take the inner value, losing the source information in the progress.
    pub fn take(self) -> T {
        self.value
    }

    /// Split the inner value and the source information.
    pub fn split(self) -> (T, ValueWithSource<()>) {
        (self.value, ValueWithSource::new((), self.source))
    }

    /// Get the source of this value, if known.
    pub fn source(&self) -> Option<&String> {
        self.source.as_ref()
    }
}

//
// Creating a `ValueWithSource`.
//

/// This is only appropriate in cases where the source isn't known. Use [`ValueWithSource::new`] or [`vws!`] for cases where it is.
impl<T> From<T> for ValueWithSource<T> {
    fn from(value: T) -> Self {
        Self {
            value,
            source: None,
        }
    }
}

/// Get a value for the current resource as a `ValueWithSource` instance.
///
/// This will drop the first element of the provided path, and use the rest as source.
///
/// ```
/// // These are equivalent.
/// vws!(self.spec.instance);
/// ValueWithSource::new(self.spec.instance, Some("spec.instance".to_owned()));
/// ```
macro_rules! vws {
    ($resource:expr, $($path:ident).+) => {
        crate::with_source::ValueWithSource::new(
            $resource.$($path).+.clone(),
            Some(stringify!($($path).+).to_owned()),
        )
    };
    ($resource:ident . $($path:ident).+) => (
        vws!($resource, $($path).+)
    );
}
pub(crate) use vws;
impl<T> ValueWithSource<T> {
    /// Create a new `ValueWithSource`.
    ///
    /// `vws!` should be used instead when possible.
    pub fn new(value: T, source: Option<String>) -> Self {
        Self { value, source }
    }
}

/// Get a nested property of a `ValueWithSource` instance.
macro_rules! vws_sub {
    ($vws:expr, $($path:ident).+) => ({
        $vws.sub($vws.$($path).+.to_owned(), stringify!($($path).+))
    });
    ($vws:ident . $($path:ident).+) => (
        vws_sub!($vws, $($path).+)
    );
}
pub(crate) use vws_sub;

impl<T> ValueWithSource<T> {
    /// Create a VWS that has a source which is derived from the source of this VWS.
    pub fn sub<V>(&self, value: V, source_suffix: &str) -> ValueWithSource<V> {
        ValueWithSource {
            value,
            source: self.source.as_ref().map(|s| format!("{s}{source_suffix}")),
        }
    }
}

//
// Traits where the `ValueWithSource` acts as its source, easing interop with things that don't care about tracking the source of value.
//

impl<T> Deref for ValueWithSource<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T> DerefMut for ValueWithSource<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

impl<T> Display for ValueWithSource<T>
where
    T: Display,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.value.fmt(f)
    }
}

impl<T> Hash for ValueWithSource<T>
where
    T: Hash,
{
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

impl<T> PartialEq for ValueWithSource<T>
where
    T: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T> PartialEq<T> for ValueWithSource<T>
where
    T: PartialEq,
{
    fn eq(&self, other: &T) -> bool {
        self.value == *other
    }
}

impl<T> Eq for ValueWithSource<T> where T: Eq
{
}

impl<T> Serialize for ValueWithSource<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.value.serialize(serializer)
    }
}

//
// Transformations.
//

impl<T> ValueWithSource<T> {
    /// Apply a transformation to the contained value while keeping the source info intact.
    pub fn transform<V, F>(self, f: F) -> ValueWithSource<V>
    where
        F: FnOnce(ValueWithSource<T>) -> V,
    {
        ValueWithSource {
            source: self.source.clone(),
            value: f(self),
        }
    }
}

impl<V> ValueWithSource<Option<V>> {
    pub fn transpose(self) -> Option<ValueWithSource<V>> {
        self.value.map(|value| ValueWithSource {
            value,
            source: self.source,
        })
    }
}

impl<V, E> ValueWithSource<Result<V, E>> {
    pub fn transpose(self) -> Result<ValueWithSource<V>, E> {
        self.value.map(|value| ValueWithSource {
            value,
            source: self.source,
        })
    }
}

impl<T> ValueWithSource<&T>
where
    T: Clone,
{
    /// Maps a `ValueWithSource<&T>` to a `ValueWithSource<T>` by cloning the contained value.
    pub fn cloned(self) -> ValueWithSource<T> {
        self.transform(|v| (*v).clone())
    }
}

//
// Iterate over underlying values as `ValueWithSource`.
//

pub struct ValueWithSourceEnumerationIterator<I: Iterator> {
    iterator: I,
    source: Option<String>,
    index: usize,
}
impl<I, V> Iterator for ValueWithSourceEnumerationIterator<I>
where
    I: Iterator<Item = V>,
{
    type Item = ValueWithSource<V>;

    fn next(&mut self) -> Option<Self::Item> {
        self.iterator.next().map(|value| {
            let index = self.index;
            self.index += 1;
            ValueWithSource::new(
                value,
                self.source
                    .as_ref()
                    .map(|source| format!("{source}[{index}]")),
            )
        })
    }
}
macro_rules! impls_for_list {
    (< $($generics:ident),* > < $type:ty >) => {
        impl<'a, $($generics),*> IntoIterator for ValueWithSource<$type> {
            type Item = <Self::IntoIter as Iterator>::Item;
            type IntoIter =
                ValueWithSourceEnumerationIterator<<$type as IntoIterator>::IntoIter>;

            fn into_iter(self) -> Self::IntoIter {
                ValueWithSourceEnumerationIterator {
                    iterator: self.value.into_iter(),
                    source: self.source,
                    index: 0,
                }
            }
        }
    };
    (< $($generics:ident),* > &'a < $type:ty >) => {
        impl<'a, $($generics),*> IntoIterator for &'a ValueWithSource<$type> {
            type Item = <Self::IntoIter as Iterator>::Item;
            type IntoIter =
                ValueWithSourceEnumerationIterator<<&'a $type as IntoIterator>::IntoIter>;

            fn into_iter(self) -> Self::IntoIter {
                ValueWithSourceEnumerationIterator {
                    iterator: (&self.value).into_iter(),
                    source: self.source.clone(),
                    index: 0,
                }
            }
        }
    };
    (< $($generics:ident),* > $type:ty) => {
        impls_for_list!(< $($generics),* > < $type >);
        impls_for_list!(< $($generics),* > < &'a $type >);
        impls_for_list!(< $($generics),* > &'a < $type >);
    };
}
impls_for_list!(<V> Vec<V>);
impls_for_list!(<V> HashSet<V>);

pub struct ValueWithSourceMapIterator<I: Iterator> {
    iterator: I,
    source: Option<String>,
}
impl<I, K, V> Iterator for ValueWithSourceMapIterator<I>
where
    K: Display,
    I: Iterator<Item = (K, V)>,
{
    type Item = (K, ValueWithSource<V>);

    fn next(&mut self) -> Option<Self::Item> {
        self.iterator.next().map(|(key, value)| {
            let source = self.source.as_ref().map(|source| format!("{source}.{key}"));
            (key, ValueWithSource::new(value, source))
        })
    }
}
macro_rules! impls_for_map {
    (< $($generics:ident),* > < $type:ty >) => {
        impl<'a, $($generics),*> IntoIterator for ValueWithSource<$type> {
            type Item = <Self::IntoIter as Iterator>::Item;
            type IntoIter =
                ValueWithSourceMapIterator<<$type as IntoIterator>::IntoIter>;

            fn into_iter(self) -> Self::IntoIter {
                ValueWithSourceMapIterator {
                    iterator: self.value.into_iter(),
                    source: self.source,
                }
            }
        }
    };
    (< $($generics:ident),* > &'a < $type:ty >) => {
        impl<'a, $($generics),*> IntoIterator for &'a ValueWithSource<$type> {
            type Item = <Self::IntoIter as Iterator>::Item;
            type IntoIter =
                ValueWithSourceMapIterator<<&'a $type as IntoIterator>::IntoIter>;

            fn into_iter(self) -> Self::IntoIter {
                ValueWithSourceMapIterator {
                    iterator: (&self.value).into_iter(),
                    source: self.source.clone(),
                }
            }
        }
    };
    (< $($generics:ident),* > $type:ty) => {
        impls_for_map!(< $($generics),* > < $type >);
        impls_for_map!(< $($generics),* > < &'a $type >);
        impls_for_map!(< $($generics),* > &'a < $type >);
    };
}
impls_for_map!(<V> HashMap<String, V>);
impls_for_map!(<> Map<String, Value>);
