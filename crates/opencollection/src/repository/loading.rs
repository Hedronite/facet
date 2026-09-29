use super::*;

pub fn load_workspace(path: impl AsRef<Path>) -> Result<LoadedWorkspace, LoadError> {
    let path = path.as_ref();
    let canonical_path = fs::canonicalize(path).map_err(|source| LoadError::Io {
        path: path.to_owned(),
        source,
    })?;
    if canonical_path.is_dir() {
        if let Some(config) = config_file(&canonical_path, "opencollection") {
            let source = read_to_string(&config)?;
            if parse(&source).is_ok_and(|parsed| parsed.is_bundled()) {
                return load_bundled_source(&source, Some(&config));
            }
        }
        load_unbundled(&canonical_path)
    } else {
        load_bundled(&canonical_path)
    }
}

/// Loads a bundled OpenCollection workspace from an in-memory YAML document.
///
/// Structural selectors are identical to selectors produced when the same bundled
/// document is loaded from a file.
pub fn load_workspace_from_str(source: &str) -> Result<LoadedWorkspace, LoadError> {
    load_bundled_source(source, None)
}

#[derive(Debug)]
enum LocatorNode {
    Folder {
        selector: String,
        children: Vec<LocatorNode>,
    },
    Request {
        selector: String,
        persistence: Option<RequestPersistence>,
    },
}

fn load_bundled(path: &Path) -> Result<LoadedWorkspace, LoadError> {
    let source = read_to_string(path)?;
    load_bundled_source(&source, Some(path))
}

fn load_bundled_source(
    source: &str,
    document_path: Option<&Path>,
) -> Result<LoadedWorkspace, LoadError> {
    let source_name = document_path.unwrap_or_else(|| Path::new("<memory>"));
    let parsed = parse(source).map_err(|source| LoadError::Parse {
        path: source_name.to_owned(),
        source,
    })?;
    if !parsed.is_bundled() {
        return Err(LoadError::InvalidMode {
            path: source_name.to_owned(),
            expected_bundled: true,
        });
    }
    let nodes = bundled_locator_nodes(parsed.document(), "items", document_path);
    let workspace = Workspace::from_collection(parsed.into_collection());
    let mut loaded = index_locators(workspace, &nodes);
    if let Some(path) = document_path {
        loaded.environment_persistence =
            bundled_environment_persistences(loaded.workspace.environments(), path);
        loaded.documents.insert(
            path.to_owned(),
            SourceDocument {
                original_source: source.as_bytes().to_vec(),
            },
        );
        loaded.source = WorkspaceSource::Bundled(path.to_owned());
    }
    Ok(loaded)
}

fn load_unbundled(root: &Path) -> Result<LoadedWorkspace, LoadError> {
    let root_config = config_file(root, "opencollection")
        .ok_or_else(|| LoadError::MissingRoot(root.to_owned()))?;
    let source = read_to_string(&root_config)?;
    let parsed = parse(&source).map_err(|source| LoadError::Parse {
        path: root_config.clone(),
        source,
    })?;
    if parsed.is_bundled() {
        return Err(LoadError::InvalidMode {
            path: root_config,
            expected_bundled: false,
        });
    }
    let mut collection = parsed.into_collection();
    let mut documents = BTreeMap::new();
    documents.insert(
        root_config.clone(),
        SourceDocument {
            original_source: source.as_bytes().to_vec(),
        },
    );
    let loaded_items = read_items(root, root, "opencollection", &mut documents)?;
    let (items, nodes): (Vec<_>, Vec<_>) = loaded_items.into_iter().unzip();
    collection.items = items;
    let mut environment_persistence =
        bundled_environment_persistences(&collection.environments, &root_config);
    let file_environments = read_environments(root, &mut documents)?;
    for (environment, path) in file_environments {
        environment_persistence.insert(
            environment.name.clone(),
            EnvironmentPersistence {
                document_path: path,
                bundled_index: None,
            },
        );
        collection.environments.push(environment);
    }
    validate_environments(&collection.environments).map_err(|error| LoadError::Validation {
        path: root.to_owned(),
        message: error.to_string(),
    })?;

    let workspace = Workspace::from_collection(collection);
    let mut loaded = index_locators(workspace, &nodes);
    loaded.environment_persistence = environment_persistence;
    loaded.documents = documents;
    loaded.source = WorkspaceSource::Unbundled(root.to_owned());
    Ok(loaded)
}

fn read_items(
    directory: &Path,
    root: &Path,
    reserved_stem: &str,
    documents: &mut BTreeMap<PathBuf, SourceDocument>,
) -> Result<Vec<(CollectionItem, LocatorNode)>, LoadError> {
    let mut entries = read_directory(directory)?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut items = Vec::new();

    for entry in entries {
        let file_type = entry.file_type().map_err(|source| LoadError::Io {
            path: entry.path(),
            source,
        })?;
        if file_type.is_symlink() {
            continue;
        }

        let path = entry.path();
        if file_type.is_dir() {
            let Some(folder_config) = config_file(&path, "folder") else {
                continue;
            };
            let read = read_item(&folder_config)?;
            documents.insert(
                folder_config.clone(),
                SourceDocument {
                    original_source: read.original_source.clone(),
                },
            );
            let mut folder = match read.item {
                Some(CollectionItem::Folder(folder)) => folder,
                Some(CollectionItem::HttpRequest(_)) => {
                    return Err(LoadError::InvalidItem {
                        path: folder_config,
                        message: "folder.yml must describe a folder".to_owned(),
                    });
                }
                None => continue,
            };
            let children = read_items(&path, root, "folder", documents)?;
            let (child_items, child_nodes): (Vec<_>, Vec<_>) = children.into_iter().unzip();
            folder.items = child_items;
            items.push((
                CollectionItem::Folder(folder),
                LocatorNode::Folder {
                    selector: relative_selector(root, &path),
                    children: child_nodes,
                },
            ));
        } else if is_yaml_file(&path)
            && path.file_stem().and_then(|stem| stem.to_str()) != Some(reserved_stem)
        {
            let read = read_item(&path)?;
            documents.insert(
                path.clone(),
                SourceDocument {
                    original_source: read.original_source.clone(),
                },
            );
            if let Some(item) = read.item {
                match item {
                    CollectionItem::HttpRequest(request) => {
                        let selector = relative_selector(root, &path);
                        items.push((
                            CollectionItem::HttpRequest(request),
                            LocatorNode::Request {
                                selector,
                                persistence: Some(RequestPersistence {
                                    document_path: path,
                                    item_path: Vec::new(),
                                }),
                            },
                        ));
                    }
                    CollectionItem::Folder(_) => {
                        return Err(LoadError::InvalidItem {
                            path,
                            message: "folders must be represented by directories with folder.yml"
                                .to_owned(),
                        });
                    }
                }
            }
        }
    }

    items.sort_by(|(left, left_node), (right, right_node)| {
        item_sequence(left)
            .total_cmp(&item_sequence(right))
            .then_with(|| locator_selector(left_node).cmp(locator_selector(right_node)))
    });
    Ok(items)
}

struct ReadItem {
    item: Option<CollectionItem>,
    original_source: Vec<u8>,
}

fn read_item(path: &Path) -> Result<ReadItem, LoadError> {
    let source = read_to_string(path)?;
    let value: Value = serde_yaml_ng::from_str(&source).map_err(|source| LoadError::Parse {
        path: path.to_owned(),
        source: ParseError::new(source),
    })?;
    let item = project_item(value).map_err(|source| LoadError::Parse {
        path: path.to_owned(),
        source: ParseError::new(source),
    })?;
    Ok(ReadItem {
        item,
        original_source: source.into_bytes(),
    })
}

fn read_environments(
    root: &Path,
    documents: &mut BTreeMap<PathBuf, SourceDocument>,
) -> Result<Vec<(Environment, PathBuf)>, LoadError> {
    let directory = root.join("environments");
    if !directory.is_dir() {
        return Ok(Vec::new());
    }

    let mut entries = read_directory(&directory)?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut environments = Vec::new();
    for entry in entries {
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|source| LoadError::Io {
                path: path.clone(),
                source,
            })?
            .is_file()
            || !is_yaml_file(&path)
        {
            continue;
        }
        let source = read_to_string(&path)?;
        let environment: EnvironmentDocument =
            serde_yaml_ng::from_str(&source).map_err(|source| LoadError::Parse {
                path: path.clone(),
                source: ParseError::new(source),
            })?;
        documents.insert(
            path.clone(),
            SourceDocument {
                original_source: source.into_bytes(),
            },
        );
        environments.push((environment.into_domain(), path));
    }
    Ok(environments)
}

fn bundled_environment_persistences(
    environments: &[Environment],
    document_path: &Path,
) -> BTreeMap<String, EnvironmentPersistence> {
    environments
        .iter()
        .enumerate()
        .map(|(index, environment)| {
            (
                environment.name.clone(),
                EnvironmentPersistence {
                    document_path: document_path.to_owned(),
                    bundled_index: Some(index),
                },
            )
        })
        .collect()
}

fn bundled_locator_nodes(
    document: &Value,
    prefix: &str,
    document_path: Option<&Path>,
) -> Vec<LocatorNode> {
    let items = document
        .get("items")
        .and_then(Value::as_sequence)
        .map_or(&[][..], Vec::as_slice);
    locator_nodes_from_items(items, prefix, document_path, &[])
}

fn locator_nodes_from_items(
    items: &[Value],
    prefix: &str,
    document_path: Option<&Path>,
    parent_path: &[usize],
) -> Vec<LocatorNode> {
    items
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            let mut item_path = parent_path.to_vec();
            item_path.push(index);
            let item_type = value
                .get("info")
                .and_then(|info| info.get("type"))
                .and_then(Value::as_str);
            match item_type {
                Some("http") => Some(LocatorNode::Request {
                    selector: format!("{prefix}/{index}"),
                    persistence: document_path.map(|path| RequestPersistence {
                        document_path: path.to_owned(),
                        item_path,
                    }),
                }),
                Some("folder") => {
                    let children = value
                        .get("items")
                        .and_then(Value::as_sequence)
                        .map_or(&[][..], Vec::as_slice);
                    Some(LocatorNode::Folder {
                        selector: format!("{prefix}/{index}"),
                        children: locator_nodes_from_items(
                            children,
                            &format!("{prefix}/{index}/items"),
                            document_path,
                            &item_path,
                        ),
                    })
                }
                _ => None,
            }
        })
        .collect()
}

fn index_locators(workspace: Workspace, nodes: &[LocatorNode]) -> LoadedWorkspace {
    let mut requests = Vec::new();
    let mut folders = Vec::new();
    index_locator_nodes(
        &workspace,
        workspace.root_items(),
        nodes,
        &mut requests,
        &mut folders,
    );
    let request_indices_by_selector = requests
        .iter()
        .enumerate()
        .map(|(index, request)| (request.selector.clone(), index))
        .collect();
    let folder_indices_by_selector = folders
        .iter()
        .enumerate()
        .map(|(index, folder)| (folder.selector.clone(), index))
        .collect();
    let request_indices_by_key = requests
        .iter()
        .enumerate()
        .map(|(index, request)| (request.key, index))
        .collect();
    let folder_indices_by_key = folders
        .iter()
        .enumerate()
        .map(|(index, folder)| (folder.key, index))
        .collect();
    LoadedWorkspace {
        workspace,
        requests,
        folders,
        request_indices_by_selector,
        folder_indices_by_selector,
        request_indices_by_key,
        folder_indices_by_key,
        environment_persistence: BTreeMap::new(),
        documents: BTreeMap::new(),
        source: WorkspaceSource::Memory,
    }
}

fn item_sequence(item: &CollectionItem) -> f64 {
    match item {
        CollectionItem::Folder(folder) => folder.metadata.sequence,
        CollectionItem::HttpRequest(request) => request.metadata.sequence,
    }
    .unwrap_or(f64::INFINITY)
}

fn locator_selector(node: &LocatorNode) -> &str {
    match node {
        LocatorNode::Folder { selector, .. } | LocatorNode::Request { selector, .. } => selector,
    }
}

fn index_locator_nodes(
    workspace: &Workspace,
    items: &[WorkspaceItemRef],
    nodes: &[LocatorNode],
    requests: &mut Vec<LocatedRequest>,
    folders: &mut Vec<LocatedFolder>,
) {
    assert_eq!(
        items.len(),
        nodes.len(),
        "locator tree must match workspace"
    );
    for (item, node) in items.iter().zip(nodes) {
        match (item, node) {
            (
                WorkspaceItemRef::Request(key),
                LocatorNode::Request {
                    selector,
                    persistence,
                },
            ) => {
                requests.push(LocatedRequest {
                    selector: selector.clone(),
                    key: *key,
                    persistence: persistence.clone(),
                });
            }
            (WorkspaceItemRef::Folder(key), LocatorNode::Folder { selector, children }) => {
                folders.push(LocatedFolder {
                    selector: selector.clone(),
                    key: *key,
                });
                let folder = workspace
                    .folder(*key)
                    .expect("workspace folder reference must resolve");
                index_locator_nodes(workspace, &folder.children, children, requests, folders);
            }
            _ => unreachable!("locator node type must match workspace item type"),
        }
    }
}

fn config_file(directory: &Path, stem: &str) -> Option<PathBuf> {
    ["yml", "yaml"]
        .into_iter()
        .map(|extension| directory.join(format!("{stem}.{extension}")))
        .find(|path| path.is_file())
}

fn read_directory(path: &Path) -> Result<Vec<fs::DirEntry>, LoadError> {
    fs::read_dir(path)
        .map_err(|source| LoadError::Io {
            path: path.to_owned(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| LoadError::Io {
            path: path.to_owned(),
            source,
        })
}

fn read_to_string(path: &Path) -> Result<String, LoadError> {
    fs::read_to_string(path).map_err(|source| LoadError::Io {
        path: path.to_owned(),
        source,
    })
}

fn is_yaml_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("yml" | "yaml")
    )
}

pub(crate) fn relative_selector(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
