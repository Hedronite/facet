//! OpenCollection YAML adapter for Probe.
//!
//! The adapter retains the source document for loss-preserving serialization and
//! projects the supported subset into serialization-independent domain models.

#![forbid(unsafe_code)]

use std::{collections::BTreeMap, error::Error as StdError, fmt, time::Duration};

use probe_core::{
    Authentication, AuthenticationKind, AuthenticationValue, Author, Body, BodyVariant, Collection,
    CollectionItem, CollectionMetadata, Environment, EnvironmentVariable, FileReference, Folder,
    FormField, Header, HttpRequest, ItemMetadata, MultipartPart, MultipartPartKind, MultipartValue,
    QueryParameter, RawBody, RawBodyKind, RequestBody, RequestSettings, SecretVariable, Variable,
    VariableValue, VariableValueSet, VariableValueType, VariableValueVariant,
    validate_environments,
};
use serde::Deserialize;
use serde_yaml_ng::Value;

mod repository;
mod structure;

pub use repository::{
    CompletedEnvironmentCreate, CompletedEnvironmentDelete, CompletedEnvironmentReplace,
    CompletedEnvironmentSave, CompletedRequestSave, CreateError, LoadError, LoadedWorkspace,
    LocatedFolder, LocatedRequest, PreparedEnvironmentCreate, PreparedEnvironmentDelete,
    PreparedEnvironmentReplace, PreparedEnvironmentSave, PreparedRequestSave, SaveError,
    create_bundled_workspace, create_bundled_workspace_from_collection, load_workspace,
    load_workspace_from_str,
};
pub use structure::{ItemKind, StructureError, StructureOperation, StructureResult};

#[cfg(test)]
mod totality_tests;

/// An OpenCollection document together with its supported domain projection.
#[derive(Clone, Debug)]
pub struct ParsedCollection {
    collection: Collection,
    document: Value,
    bundled: bool,
}

impl ParsedCollection {
    pub(crate) const fn document(&self) -> &Value {
        &self.document
    }

    pub(crate) const fn is_bundled(&self) -> bool {
        self.bundled
    }

    /// Returns the serialization-independent collection model.
    #[must_use]
    pub const fn collection(&self) -> &Collection {
        &self.collection
    }

    /// Consumes the parsed document and returns its domain model.
    #[must_use]
    pub fn into_collection(self) -> Collection {
        self.collection
    }

    /// Serializes the retained OpenCollection document back to YAML.
    ///
    /// Unsupported fields are emitted from the retained document rather than rebuilt
    /// from the supported domain projection.
    pub fn to_yaml(&self) -> Result<String, ParseError> {
        serde_yaml_ng::to_string(&self.document).map_err(ParseError::new)
    }
}

/// An error raised while parsing or serializing OpenCollection YAML.
#[derive(Debug)]
pub struct ParseError {
    source: serde_yaml_ng::Error,
}

impl ParseError {
    fn new(source: serde_yaml_ng::Error) -> Self {
        Self { source }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid OpenCollection YAML: {}", self.source)
    }
}

impl StdError for ParseError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.source)
    }
}

/// Parses a bundled OpenCollection YAML document.
///
/// Items outside the currently supported folder and HTTP request subset remain in
/// the retained YAML document but are not projected into the domain model.
pub fn parse(source: &str) -> Result<ParsedCollection, ParseError> {
    let document: Value = serde_yaml_ng::from_str(source).map_err(ParseError::new)?;
    let wire: CollectionDocument =
        serde_yaml_ng::from_value(document.clone()).map_err(ParseError::new)?;
    if wire.opencollection != "1.0.0" {
        return Err(ParseError::new(
            <serde_yaml_ng::Error as serde::de::Error>::custom(format!(
                "unsupported OpenCollection version '{}'; supported version is 1.0.0",
                wire.opencollection
            )),
        ));
    }
    let bundled = wire.bundled;
    let collection = wire.into_domain().map_err(ParseError::new)?;
    validate_environments(&collection.environments).map_err(|error| {
        ParseError::new(<serde_yaml_ng::Error as serde::de::Error>::custom(
            error.to_string(),
        ))
    })?;

    Ok(ParsedCollection {
        collection,
        document,
        bundled,
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CollectionDocument {
    opencollection: String,
    info: CollectionInfoDocument,
    bundled: bool,
    #[serde(default)]
    items: Vec<Value>,
    #[serde(default)]
    config: CollectionConfigDocument,
}

impl CollectionDocument {
    fn into_domain(self) -> Result<Collection, serde_yaml_ng::Error> {
        Ok(Collection {
            metadata: self.info.into_domain(),
            items: project_items(self.items)?,
            environments: self
                .config
                .environments
                .into_iter()
                .map(EnvironmentDocument::into_domain)
                .collect(),
        })
    }
}

#[derive(Debug, Default, Deserialize)]
struct CollectionConfigDocument {
    #[serde(default)]
    environments: Vec<EnvironmentDocument>,
}

#[derive(Debug, Default, Deserialize)]
struct CollectionInfoDocument {
    name: Option<String>,
    summary: Option<String>,
    version: Option<String>,
    #[serde(default)]
    authors: Vec<AuthorDocument>,
}

impl CollectionInfoDocument {
    fn into_domain(self) -> CollectionMetadata {
        CollectionMetadata {
            name: self.name,
            summary: self.summary,
            version: self.version,
            authors: self
                .authors
                .into_iter()
                .map(AuthorDocument::into_domain)
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct AuthorDocument {
    name: Option<String>,
    email: Option<String>,
    url: Option<String>,
}

impl AuthorDocument {
    fn into_domain(self) -> Author {
        Author {
            name: self.name,
            email: self.email,
            url: self.url,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ItemInfoDocument {
    name: Option<String>,
    seq: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct ItemKindDocument {
    #[serde(default)]
    info: ItemKindInfoDocument,
}

#[derive(Debug, Default, Deserialize)]
struct ItemKindInfoDocument {
    #[serde(rename = "type")]
    item_type: Option<String>,
}

impl ItemInfoDocument {
    fn into_domain(self) -> ItemMetadata {
        ItemMetadata {
            name: self.name,
            sequence: self.seq,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ItemDocument {
    #[serde(default)]
    info: ItemInfoDocument,
    #[serde(default)]
    items: Vec<Value>,
    http: Option<HttpDetailsDocument>,
    #[serde(default)]
    settings: RequestSettingsDocument,
}

#[derive(Debug, Default, Deserialize)]
struct RequestSettingsDocument {
    timeout: Option<Value>,
    #[serde(rename = "followRedirects")]
    follow_redirects: Option<bool>,
    #[serde(rename = "maxRedirects")]
    max_redirects: Option<usize>,
}

impl RequestSettingsDocument {
    fn into_domain(self) -> Result<RequestSettings, serde_yaml_ng::Error> {
        let timeout = match self.timeout {
            None => None,
            Some(Value::String(value)) if value == "inherit" => None,
            Some(Value::Number(value)) => {
                let milliseconds = value.as_f64().ok_or_else(|| {
                    <serde_yaml_ng::Error as serde::de::Error>::custom(
                        "request timeout must be a finite non-negative number",
                    )
                })?;
                if milliseconds.is_sign_negative() || !milliseconds.is_finite() {
                    return Err(<serde_yaml_ng::Error as serde::de::Error>::custom(
                        "request timeout must be a finite non-negative number",
                    ));
                }
                Some(
                    Duration::try_from_secs_f64(milliseconds / 1000.0).map_err(|_| {
                        <serde_yaml_ng::Error as serde::de::Error>::custom(
                            "request timeout is too large",
                        )
                    })?,
                )
            }
            Some(_) => {
                return Err(<serde_yaml_ng::Error as serde::de::Error>::custom(
                    "request timeout must be milliseconds or 'inherit'",
                ));
            }
        };
        Ok(RequestSettings {
            timeout,
            follow_redirects: self.follow_redirects,
            max_redirects: self.max_redirects,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
struct HttpDetailsDocument {
    method: Option<String>,
    url: Option<String>,
    #[serde(default)]
    headers: Vec<HeaderDocument>,
    #[serde(default)]
    params: Vec<ParameterDocument>,
    body: Option<Value>,
    auth: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct HeaderDocument {
    name: String,
    value: String,
    #[serde(default)]
    disabled: bool,
}

impl HeaderDocument {
    fn into_domain(self) -> Header {
        Header {
            name: self.name,
            value: self.value,
            disabled: self.disabled,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ParameterDocument {
    name: String,
    value: String,
    #[serde(rename = "type")]
    parameter_type: String,
    #[serde(default)]
    disabled: bool,
}

impl ParameterDocument {
    fn into_domain(self) -> QueryParameter {
        QueryParameter {
            name: self.name,
            value: self.value,
            disabled: self.disabled,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentDocument {
    name: String,
    color: Option<String>,
    extends: Option<String>,
    dot_env_file_path: Option<String>,
    #[serde(default)]
    variables: Vec<EnvironmentVariableDocument>,
}

impl EnvironmentDocument {
    fn into_domain(self) -> Environment {
        Environment {
            name: self.name,
            color: self.color,
            extends: self.extends,
            dot_env_file_path: self.dot_env_file_path,
            variables: self
                .variables
                .into_iter()
                .map(EnvironmentVariableDocument::into_domain)
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct EnvironmentVariableDocument {
    name: Option<String>,
    value: Option<VariableValueSetDocument>,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    secret: bool,
    #[serde(rename = "type")]
    value_type: Option<VariableValueTypeDocument>,
}

impl EnvironmentVariableDocument {
    fn into_domain(self) -> EnvironmentVariable {
        if self.secret {
            EnvironmentVariable::Secret(SecretVariable {
                name: self.name,
                value_type: self.value_type.map(VariableValueTypeDocument::into_domain),
                disabled: self.disabled,
            })
        } else {
            EnvironmentVariable::Plain(Variable {
                name: self.name,
                value: self.value.map(VariableValueSetDocument::into_domain),
                disabled: self.disabled,
            })
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum VariableValueSetDocument {
    String(String),
    Typed(TypedVariableValueDocument),
    Variants(Vec<VariableValueVariantDocument>),
}

impl VariableValueSetDocument {
    fn into_domain(self) -> VariableValueSet {
        match self {
            Self::String(value) => VariableValueSet::Single(VariableValue::String(value)),
            Self::Typed(value) => VariableValueSet::Single(value.into_domain()),
            Self::Variants(variants) => VariableValueSet::Variants(
                variants
                    .into_iter()
                    .map(VariableValueVariantDocument::into_domain)
                    .collect(),
            ),
        }
    }
}

#[derive(Debug, Deserialize)]
struct TypedVariableValueDocument {
    #[serde(rename = "type")]
    value_type: VariableValueTypeDocument,
    data: String,
}

impl TypedVariableValueDocument {
    fn into_domain(self) -> VariableValue {
        VariableValue::Typed {
            kind: self.value_type.into_domain(),
            data: self.data,
        }
    }
}

#[derive(Debug, Deserialize)]
struct VariableValueVariantDocument {
    title: String,
    #[serde(default)]
    selected: bool,
    value: VariableValueDocument,
}

impl VariableValueVariantDocument {
    fn into_domain(self) -> VariableValueVariant {
        VariableValueVariant {
            title: self.title,
            selected: self.selected,
            value: self.value.into_domain(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum VariableValueDocument {
    String(String),
    Typed(TypedVariableValueDocument),
}

impl VariableValueDocument {
    fn into_domain(self) -> VariableValue {
        match self {
            Self::String(value) => VariableValue::String(value),
            Self::Typed(value) => value.into_domain(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum VariableValueTypeDocument {
    String,
    Number,
    Boolean,
    Null,
    Object,
}

impl VariableValueTypeDocument {
    const fn into_domain(self) -> VariableValueType {
        match self {
            Self::String => VariableValueType::String,
            Self::Number => VariableValueType::Number,
            Self::Boolean => VariableValueType::Boolean,
            Self::Null => VariableValueType::Null,
            Self::Object => VariableValueType::Object,
        }
    }
}

#[derive(Debug, Deserialize)]
struct BodyKindDocument {
    #[serde(rename = "type")]
    body_type: String,
}

#[derive(Debug, Deserialize)]
struct RawBodyDocument {
    #[serde(rename = "type")]
    body_type: RawBodyKindDocument,
    data: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawBodyKindDocument {
    Json,
    Text,
    Xml,
    Sparql,
}

impl RawBodyKindDocument {
    const fn into_domain(self) -> RawBodyKind {
        match self {
            Self::Json => RawBodyKind::Json,
            Self::Text => RawBodyKind::Text,
            Self::Xml => RawBodyKind::Xml,
            Self::Sparql => RawBodyKind::Sparql,
        }
    }
}

#[derive(Debug, Deserialize)]
struct FormBodyDocument {
    data: Vec<FormFieldDocument>,
}

#[derive(Debug, Deserialize)]
struct FormFieldDocument {
    name: String,
    value: String,
    #[serde(default)]
    disabled: bool,
}

impl FormFieldDocument {
    fn into_domain(self) -> FormField {
        FormField {
            name: self.name,
            value: self.value,
            disabled: self.disabled,
        }
    }
}

#[derive(Debug, Deserialize)]
struct MultipartBodyDocument {
    data: Vec<MultipartPartDocument>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MultipartPartDocument {
    name: String,
    #[serde(rename = "type")]
    part_type: MultipartPartKindDocument,
    value: MultipartValueDocument,
    content_type: Option<String>,
    #[serde(default)]
    disabled: bool,
}

impl MultipartPartDocument {
    fn into_domain(self) -> MultipartPart {
        MultipartPart {
            name: self.name,
            kind: self.part_type.into_domain(),
            value: self.value.into_domain(),
            content_type: self.content_type,
            disabled: self.disabled,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum MultipartPartKindDocument {
    Text,
    File,
}

impl MultipartPartKindDocument {
    const fn into_domain(self) -> MultipartPartKind {
        match self {
            Self::Text => MultipartPartKind::Text,
            Self::File => MultipartPartKind::File,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MultipartValueDocument {
    Single(String),
    Multiple(Vec<String>),
}

impl MultipartValueDocument {
    fn into_domain(self) -> MultipartValue {
        match self {
            Self::Single(value) => MultipartValue::Single(value),
            Self::Multiple(values) => MultipartValue::Multiple(values),
        }
    }
}

#[derive(Debug, Deserialize)]
struct FileBodyDocument {
    data: Vec<FileReferenceDocument>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileReferenceDocument {
    file_path: String,
    content_type: String,
    selected: bool,
}

impl FileReferenceDocument {
    fn into_domain(self) -> FileReference {
        FileReference {
            file_path: self.file_path,
            content_type: self.content_type,
            selected: self.selected,
        }
    }
}

#[derive(Debug, Deserialize)]
struct BodyVariantDocument {
    title: String,
    #[serde(default)]
    selected: bool,
    body: Value,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AuthenticationDocument {
    Inherit(String),
    Scheme(AuthenticationSchemeDocument),
}

#[derive(Debug, Deserialize)]
struct AuthenticationSchemeDocument {
    #[serde(rename = "type")]
    authentication_type: String,
    #[serde(flatten)]
    properties: BTreeMap<String, Value>,
}

fn project_request_body(value: Value) -> Result<Option<RequestBody>, serde_yaml_ng::Error> {
    if value.is_sequence() {
        let variants: Vec<BodyVariantDocument> = serde_yaml_ng::from_value(value)?;
        let mut projected = Vec::with_capacity(variants.len());

        for variant in variants {
            if let Some(body) = project_body(variant.body)? {
                projected.push(BodyVariant {
                    title: variant.title,
                    selected: variant.selected,
                    body,
                });
            }
        }

        Ok(Some(RequestBody::Variants(projected)))
    } else {
        Ok(project_body(value)?.map(RequestBody::Single))
    }
}

fn project_body(value: Value) -> Result<Option<Body>, serde_yaml_ng::Error> {
    let kind: BodyKindDocument = serde_yaml_ng::from_value(value.clone())?;

    match kind.body_type.as_str() {
        "json" | "text" | "xml" | "sparql" => {
            let body: RawBodyDocument = serde_yaml_ng::from_value(value)?;
            Ok(Some(Body::Raw(RawBody {
                kind: body.body_type.into_domain(),
                data: body.data,
            })))
        }
        "form-urlencoded" => {
            let body: FormBodyDocument = serde_yaml_ng::from_value(value)?;
            Ok(Some(Body::FormUrlEncoded(
                body.data
                    .into_iter()
                    .map(FormFieldDocument::into_domain)
                    .collect(),
            )))
        }
        "multipart-form" => {
            let body: MultipartBodyDocument = serde_yaml_ng::from_value(value)?;
            Ok(Some(Body::Multipart(
                body.data
                    .into_iter()
                    .map(MultipartPartDocument::into_domain)
                    .collect(),
            )))
        }
        "file" => {
            let body: FileBodyDocument = serde_yaml_ng::from_value(value)?;
            Ok(Some(Body::File(
                body.data
                    .into_iter()
                    .map(FileReferenceDocument::into_domain)
                    .collect(),
            )))
        }
        _ => Ok(None),
    }
}

fn project_authentication(value: Value) -> Result<Authentication, serde_yaml_ng::Error> {
    let auth: AuthenticationDocument = serde_yaml_ng::from_value(value)?;

    Ok(match auth {
        AuthenticationDocument::Inherit(value) => Authentication {
            kind: if value == "inherit" {
                AuthenticationKind::Inherit
            } else {
                AuthenticationKind::Other(value)
            },
            properties: BTreeMap::new(),
        },
        AuthenticationDocument::Scheme(auth) => Authentication {
            kind: match auth.authentication_type.as_str() {
                "awsv4" => AuthenticationKind::AwsV4,
                "basic" => AuthenticationKind::Basic,
                "wsse" => AuthenticationKind::Wsse,
                "bearer" => AuthenticationKind::Bearer,
                "digest" => AuthenticationKind::Digest,
                "ntlm" => AuthenticationKind::Ntlm,
                "apikey" => AuthenticationKind::ApiKey,
                "oauth1" => AuthenticationKind::OAuth1,
                "oauth2" => AuthenticationKind::OAuth2,
                other => AuthenticationKind::Other(other.to_owned()),
            },
            properties: auth
                .properties
                .into_iter()
                .map(|(name, value)| (name, authentication_value(value)))
                .collect(),
        },
    })
}

fn authentication_value(value: Value) -> AuthenticationValue {
    match value {
        Value::Null => AuthenticationValue::Null,
        Value::Bool(value) => AuthenticationValue::Boolean(value),
        Value::Number(value) => AuthenticationValue::Number(value.to_string()),
        Value::String(value) => AuthenticationValue::String(value),
        Value::Sequence(values) => {
            AuthenticationValue::Sequence(values.into_iter().map(authentication_value).collect())
        }
        Value::Mapping(values) => AuthenticationValue::Object(
            values
                .into_iter()
                .filter_map(|(name, value)| {
                    name.as_str()
                        .map(str::to_owned)
                        .map(|name| (name, authentication_value(value)))
                })
                .collect(),
        ),
        Value::Tagged(value) => authentication_value(value.value),
    }
}

fn project_items(items: Vec<Value>) -> Result<Vec<CollectionItem>, serde_yaml_ng::Error> {
    items
        .into_iter()
        .map(project_item)
        .filter_map(Result::transpose)
        .collect()
}

fn project_item(value: Value) -> Result<Option<CollectionItem>, serde_yaml_ng::Error> {
    let kind: ItemKindDocument = serde_yaml_ng::from_value(value.clone())?;

    match kind.info.item_type.as_deref() {
        Some("folder") => {
            let item: ItemDocument = serde_yaml_ng::from_value(value)?;
            Ok(Some(CollectionItem::Folder(Folder {
                metadata: item.info.into_domain(),
                items: project_items(item.items)?,
            })))
        }
        Some("http") => {
            let item: ItemDocument = serde_yaml_ng::from_value(value)?;
            let settings = item.settings.into_domain()?;
            let http = item.http.unwrap_or_default();
            let body = http.body.map(project_request_body).transpose()?.flatten();
            let authentication = http.auth.map(project_authentication).transpose()?;
            let mut query_parameters = Vec::new();
            let mut path_parameters = Vec::new();
            for parameter in http.params {
                match parameter.parameter_type.as_str() {
                    "query" => query_parameters.push(parameter.into_domain()),
                    "path" => path_parameters.push(parameter.into_domain()),
                    _ => {}
                }
            }
            Ok(Some(CollectionItem::HttpRequest(HttpRequest {
                metadata: item.info.into_domain(),
                method: http.method,
                url: http.url,
                headers: http
                    .headers
                    .into_iter()
                    .map(HeaderDocument::into_domain)
                    .collect(),
                query_parameters,
                path_parameters,
                body,
                authentication,
                settings,
            })))
        }
        _ => Ok(None),
    }
}
