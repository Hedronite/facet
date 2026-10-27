use std::collections::{BTreeMap, BTreeSet};

use crate::{
    AuthenticationValue, Body, Environment, EnvironmentResolutionError, HttpRequest,
    MultipartValue, RequestBody, ResolvedEnvironment,
    environment::{EffectiveVariableDeclaration, effective_variable_declarations},
};

/// A stable, human-oriented location where a request references a variable.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum VariableUsage {
    /// The HTTP method.
    Method,
    /// The request URL.
    Url,
    /// A header name or value.
    Header { name: String },
    /// A query-parameter name or value.
    QueryParameter { name: String },
    /// A path-parameter name or value.
    PathParameter { name: String },
    /// A raw request body.
    Body,
    /// A form-urlencoded field name or value.
    FormUrlEncoded { name: String },
    /// A multipart part name, value, or content type.
    Multipart { name: String },
    /// A file-body path or content type.
    File,
    /// An authentication property, including values nested below it.
    Authentication { name: String },
}

/// Metadata about one variable referenced by a request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestVariableInfo {
    /// Interpolation variable name.
    pub name: String,
    /// Whether the selected effective environment declares an enabled variable with this name.
    pub defined: bool,
    /// Whether that effective declaration is a secret.
    pub secret: bool,
    /// Deterministically ordered, deduplicated request locations.
    pub usages: Vec<VariableUsage>,
}

/// Discovers request variable references without resolving their values.
///
/// A selected environment is inspected through the same validated inheritance chain used by
/// normal resolution. Disabled declarations shadow inherited declarations but are not defined.
/// No variable values (including secrets) are read or interpolated.
pub fn discover_request_variables(
    request: &HttpRequest,
    environments: &[Environment],
    selected: Option<&str>,
) -> Result<Vec<RequestVariableInfo>, EnvironmentResolutionError> {
    let declarations = effective_variable_declarations(environments, selected)?;
    let mut usages_by_name = BTreeMap::<String, BTreeSet<VariableUsage>>::new();
    let mut request = request.clone();
    transform_request_strings(&mut request, |value, usage| {
        for name in interpolation_references(value)? {
            usages_by_name
                .entry(name)
                .or_default()
                .insert(usage.clone());
        }
        Ok(())
    })?;

    Ok(usages_by_name
        .into_iter()
        .map(|(name, usages)| {
            let declaration = declarations.get(&name).copied();
            let defined = matches!(
                declaration,
                Some(EffectiveVariableDeclaration::Plain | EffectiveVariableDeclaration::Secret)
            );
            RequestVariableInfo {
                name,
                defined,
                secret: matches!(declaration, Some(EffectiveVariableDeclaration::Secret)),
                usages: usages.into_iter().collect(),
            }
        })
        .collect())
}

/// Transport-locking classification for a request about to leave the
/// process, decided from variable names only. No variable value enters the
/// decision or an error.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TransportLock {
    /// Discovery classified every referenced variable: with a selected
    /// environment none is declared secret, without one none is supplied by
    /// `--var` or a store row.
    #[default]
    Open,
    /// An environment is selected and a referenced variable is declared
    /// secret, whatever its source.
    SecretReferenced,
    /// No environment is selected and a referenced variable is supplied by
    /// `--var` or a store row, so its secret status cannot be classified.
    VarUnclassified,
    /// Variable discovery failed, so the request cannot be classified.
    DiscoveryFailed,
}

impl TransportLock {
    /// Every classification except `Open` must keep locked transport.
    #[must_use]
    pub fn is_locked(self) -> bool {
        !matches!(self, Self::Open)
    }
}

/// Decides the transport lock for `request` from variable names only.
///
/// Discovery failure is its own locked branch: the request cannot be
/// classified, so it locks. With a selected environment the effective
/// declared-secret declarations decide; without one, every reference that
/// `supplied` reports as coming from `--var` or a store row locks as
/// unclassified.
pub fn transport_lock(
    request: &HttpRequest,
    environments: &[Environment],
    selected: Option<&str>,
    supplied: impl Fn(&str) -> bool,
) -> TransportLock {
    let Ok(referenced) = discover_request_variables(request, environments, selected) else {
        return TransportLock::DiscoveryFailed;
    };
    match selected {
        Some(_) => {
            let any_secret = referenced.iter().any(|info| info.secret);
            if any_secret {
                TransportLock::SecretReferenced
            } else {
                TransportLock::Open
            }
        }
        None => {
            let any_supplied = referenced.iter().any(|info| supplied(&info.name));
            if any_supplied {
                TransportLock::VarUnclassified
            } else {
                TransportLock::Open
            }
        }
    }
}

/// Clones a request and interpolates every currently supported request-value field.
pub fn resolve_request(
    request: &HttpRequest,
    environment: &ResolvedEnvironment,
) -> Result<HttpRequest, EnvironmentResolutionError> {
    resolve_request_with(request, |value| environment.interpolate(value))
}

/// Clones a request and interpolates every supported request-value field, rejecting
/// references that do not have an available value.
pub fn resolve_request_strict(
    request: &HttpRequest,
    environment: &ResolvedEnvironment,
) -> Result<HttpRequest, EnvironmentResolutionError> {
    resolve_request_with(request, |value| environment.interpolate_strict(value))
}

fn resolve_request_with(
    request: &HttpRequest,
    interpolate: impl Fn(&str) -> Result<String, EnvironmentResolutionError>,
) -> Result<HttpRequest, EnvironmentResolutionError> {
    let mut request = request.clone();
    transform_request_strings(&mut request, |value, _usage| {
        *value = interpolate(value)?;
        Ok(())
    })?;
    Ok(request)
}

fn transform_request_strings<E>(
    request: &mut HttpRequest,
    mut transform: impl FnMut(&mut String, &VariableUsage) -> Result<(), E>,
) -> Result<(), E> {
    if let Some(method) = &mut request.method {
        transform(method, &VariableUsage::Method)?;
    }
    if let Some(url) = &mut request.url {
        transform(url, &VariableUsage::Url)?;
    }
    for header in &mut request.headers {
        let usage = VariableUsage::Header {
            name: header.name.clone(),
        };
        transform(&mut header.name, &usage)?;
        transform(&mut header.value, &usage)?;
    }
    for parameter in &mut request.query_parameters {
        let usage = VariableUsage::QueryParameter {
            name: parameter.name.clone(),
        };
        transform(&mut parameter.name, &usage)?;
        transform(&mut parameter.value, &usage)?;
    }
    for parameter in &mut request.path_parameters {
        let usage = VariableUsage::PathParameter {
            name: parameter.name.clone(),
        };
        transform(&mut parameter.name, &usage)?;
        transform(&mut parameter.value, &usage)?;
    }
    if let Some(body) = &mut request.body {
        transform_body(body, &mut transform)?;
    }
    if let Some(authentication) = &mut request.authentication {
        for (name, value) in &mut authentication.properties {
            let usage = VariableUsage::Authentication { name: name.clone() };
            transform_authentication_value(value, &usage, &mut transform)?;
        }
    }
    Ok(())
}

fn transform_body<E>(
    body: &mut RequestBody,
    transform: &mut impl FnMut(&mut String, &VariableUsage) -> Result<(), E>,
) -> Result<(), E> {
    match body {
        RequestBody::Single(body) => transform_body_value(body, transform),
        RequestBody::Variants(variants) => {
            for variant in variants {
                transform_body_value(&mut variant.body, transform)?;
            }
            Ok(())
        }
    }
}

fn transform_body_value<E>(
    body: &mut Body,
    transform: &mut impl FnMut(&mut String, &VariableUsage) -> Result<(), E>,
) -> Result<(), E> {
    match body {
        Body::Raw(body) => transform(&mut body.data, &VariableUsage::Body)?,
        Body::FormUrlEncoded(fields) => {
            for field in fields {
                let usage = VariableUsage::FormUrlEncoded {
                    name: field.name.clone(),
                };
                transform(&mut field.name, &usage)?;
                transform(&mut field.value, &usage)?;
            }
        }
        Body::Multipart(parts) => {
            for part in parts {
                let usage = VariableUsage::Multipart {
                    name: part.name.clone(),
                };
                transform(&mut part.name, &usage)?;
                match &mut part.value {
                    MultipartValue::Single(value) => transform(value, &usage)?,
                    MultipartValue::Multiple(values) => {
                        for value in values {
                            transform(value, &usage)?;
                        }
                    }
                }
                if let Some(content_type) = &mut part.content_type {
                    transform(content_type, &usage)?;
                }
            }
        }
        Body::File(files) => {
            for file in files {
                transform(&mut file.file_path, &VariableUsage::File)?;
                transform(&mut file.content_type, &VariableUsage::File)?;
            }
        }
    }
    Ok(())
}

fn transform_authentication_value<E>(
    value: &mut AuthenticationValue,
    usage: &VariableUsage,
    transform: &mut impl FnMut(&mut String, &VariableUsage) -> Result<(), E>,
) -> Result<(), E> {
    match value {
        AuthenticationValue::String(value) | AuthenticationValue::Number(value) => {
            transform(value, usage)?
        }
        AuthenticationValue::Sequence(values) => {
            for value in values {
                transform_authentication_value(value, usage, transform)?;
            }
        }
        AuthenticationValue::Object(values) => {
            for value in values.values_mut() {
                transform_authentication_value(value, usage, transform)?;
            }
        }
        AuthenticationValue::Boolean(_) | AuthenticationValue::Null => {}
    }
    Ok(())
}

fn interpolation_references(input: &str) -> Result<Vec<String>, EnvironmentResolutionError> {
    let mut references = Vec::new();
    crate::environment::interpolate(input, |name| {
        references.push(name.to_owned());
        Ok(Some(String::new()))
    })?;
    Ok(references)
}
