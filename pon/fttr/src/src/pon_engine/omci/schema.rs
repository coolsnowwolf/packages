use serde::Serialize;

/// OMCI-005: G.988 kinds retained independently of their wire widths.
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/generated/attribute.go#L42-L76>
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributeKind {
    Unknown,
    Octets,
    String,
    UnsignedInteger,
    Table,
    SignedInteger,
    Pointer,
    BitField,
    Enumeration,
    Counter,
}

/// Stable reference IDs and links are part of the frontend contract (project design).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Reference {
    pub id: &'static str,
    pub url: &'static str,
}

/// OMCI-005: index, mask, size, access and defaults are imported from the pinned schema.
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/generated/attribute.go#L62-L76>
#[derive(Debug, Clone, Copy, Serialize)]
pub struct AttributeDefinition {
    pub index: u8,
    pub name: &'static str,
    pub mask: u16,
    pub kind: AttributeKind,
    pub size: usize,
    pub read: bool,
    pub write: bool,
    pub set_by_create: bool,
    pub avc: bool,
    pub optional: bool,
    pub deprecated: bool,
    pub default_hex: Option<&'static str>,
    pub reference: Reference,
}

/// OMCI-004: composition replaces Go's embedded ManagedEntityDefinition.
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/generated/medef.go#L31-L42>
#[derive(Debug, Clone, Copy, Serialize)]
pub struct MeDefinition {
    pub class_id: u16,
    pub name: &'static str,
    pub allowed_mask: u16,
    pub access: &'static str,
    pub actions: &'static [u8],
    pub attributes: &'static [AttributeDefinition],
    pub layout_supported: bool,
    pub reference: Reference,
}
impl MeDefinition {
    pub fn attribute(&self, index: u8) -> Option<&'static AttributeDefinition> {
        self.attributes.iter().find(|a| a.index == index)
    }
    pub fn named(&self, name: &str) -> Option<&'static AttributeDefinition> {
        self.attributes.iter().find(|a| a.name == name)
    }
}

/// OMCI-006: immutable, sorted class registry; unselected classes have names only.
/// <https://github.com/opencord/omci-lib-go/blob/v2.2.4/generated/classidmap.go>
pub struct MeRegistry;
impl MeRegistry {
    pub fn get(class_id: u16) -> Option<&'static MeDefinition> {
        super::generated::DEFINITIONS
            .binary_search_by_key(&class_id, |d| d.class_id)
            .ok()
            .map(|i| &super::generated::DEFINITIONS[i])
    }
    pub fn all() -> &'static [MeDefinition] {
        super::generated::DEFINITIONS
    }
}
