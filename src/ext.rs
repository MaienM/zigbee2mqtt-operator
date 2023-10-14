use kube::{Resource, ResourceExt};

pub trait ResourceLocalExt {
    /// Get a description of the type of the resource in the form {api_version}/{kind}.
    fn kind_with_version() -> String;
    /// Get the name including the namespace for this resource.
    fn full_name(&self) -> String;
    /// Get a string that uniquely represent this resouce in the form of {kind_with_version}{full_name}.
    fn id(&self) -> String;
}
impl<T> ResourceLocalExt for T
where
    T: Resource,
    <T as Resource>::DynamicType: Default,
{
    fn kind_with_version() -> String {
        let dt = <Self as Resource>::DynamicType::default();
        format!(
            "{api_version}/{kind}",
            api_version = Self::api_version(&dt),
            kind = Self::kind(&dt),
        )
    }

    fn full_name(&self) -> String {
        let name = self.name_any();
        match self.namespace() {
            Some(namespace) => format!("{namespace}/{name}"),
            None => name,
        }
    }

    fn id(&self) -> String {
        format!(
            "{kind_with_version}/{full_name}",
            kind_with_version = Self::kind_with_version(),
            full_name = self.full_name(),
        )
    }
}
