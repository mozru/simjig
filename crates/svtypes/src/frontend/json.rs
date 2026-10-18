//! Path-tracking access to a Verilator `.tree.json` document.
//!
//! Verilator links nodes by `addr` strings (`"(E)"`) that are only meaningful inside one
//! file, so the [`Index`] maps every `addr` to its node and the node's JSON pointer. Every
//! accessor reports failures as [`Error::Json`] carrying that pointer, the source location
//! and the Verilator version, which is what makes strict decoding debuggable.

use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::{Map, Value};

use crate::error::Error;

/// A JSON object node together with its JSON pointer.
#[derive(Clone, Debug)]
pub(crate) struct Node<'a> {
    pub(crate) obj: &'a Map<String, Value>,
    pub(crate) path: Rc<str>,
}

/// Fields every node may carry; they never change the meaning of a type.
const COMMON_FIELDS: [&str; 6] = ["type", "name", "addr", "loc", "dtypep", "editNum"];

/// All nodes of one document, by `addr`, plus what errors need to say.
#[derive(Debug)]
pub(crate) struct Index<'a> {
    nodes: BTreeMap<&'a str, Node<'a>>,
    /// Source file id (`"e"`) to file name, from `.tree.meta.json`.
    files: BTreeMap<String, String>,
    pub(crate) version: String,
}

impl<'a> Index<'a> {
    /// Indexes every node of `root` that has an `addr`.
    pub(crate) fn new(root: &'a Value, meta: &Value, version: &str) -> Index<'a> {
        let mut nodes = BTreeMap::new();
        collect(root, &mut String::new(), &mut nodes);
        let files = meta
            .get("files")
            .and_then(Value::as_object)
            .map(|files| {
                files
                    .iter()
                    .filter_map(|(id, f)| {
                        let name = f.get("filename")?.as_str()?;
                        Some((id.clone(), name.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Index {
            nodes,
            files,
            version: version.to_owned(),
        }
    }

    /// A [`Error::Json`] about `node`.
    pub(crate) fn err(&self, node: &Node<'_>, message: impl Into<String>) -> Error {
        Error::Json {
            path: if node.path.is_empty() {
                "/".into()
            } else {
                node.path.to_string()
            },
            source_loc: self.source_loc(node),
            version: self.version.clone(),
            message: message.into(),
        }
    }

    /// `file:line:col` of a node's `loc` (`"e,13:8,13:9"`).
    pub(crate) fn source_loc(&self, node: &Node<'_>) -> Option<String> {
        let loc = node.obj.get("loc")?.as_str()?;
        let (file, rest) = loc.split_once(',')?;
        let start = rest.split(',').next()?;
        let name = self.files.get(file).map_or(file, String::as_str);
        Some(format!("{name}:{start}"))
    }

    /// The node's `type`.
    pub(crate) fn kind<'n>(node: &'n Node<'a>) -> &'n str {
        node.obj.get("type").and_then(Value::as_str).unwrap_or("")
    }

    /// A required string field.
    pub(crate) fn str(&self, node: &Node<'a>, key: &str) -> Result<&'a str, Error> {
        match node.obj.get(key) {
            Some(Value::String(s)) => Ok(s),
            Some(_) => Err(self.err(node, format!("field `{key}` is not a string"))),
            None => Err(self.err(node, format!("{} has no field `{key}`", Index::kind(node)))),
        }
    }

    /// An optional string field.
    pub(crate) fn opt_str(&self, node: &Node<'a>, key: &str) -> Result<Option<&'a str>, Error> {
        match node.obj.get(key) {
            None => Ok(None),
            Some(_) => self.str(node, key).map(Some),
        }
    }

    /// A flag; Verilator omits flags that are false.
    pub(crate) fn flag(&self, node: &Node<'a>, key: &str) -> Result<bool, Error> {
        match node.obj.get(key) {
            None => Ok(false),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => Err(self.err(node, format!("field `{key}` is not a boolean"))),
        }
    }

    /// An optional integer field.
    pub(crate) fn opt_int(&self, node: &Node<'a>, key: &str) -> Result<Option<i64>, Error> {
        match node.obj.get(key) {
            None => Ok(None),
            Some(v) => v
                .as_i64()
                .map(Some)
                .ok_or_else(|| self.err(node, format!("field `{key}` is not an integer"))),
        }
    }

    /// The child nodes in list field `key` (absent means empty).
    pub(crate) fn children(&self, node: &Node<'a>, key: &str) -> Result<Vec<Node<'a>>, Error> {
        let Some(value) = node.obj.get(key) else {
            return Ok(Vec::new());
        };
        let list = value
            .as_array()
            .ok_or_else(|| self.err(node, format!("field `{key}` is not a list")))?;
        list.iter()
            .enumerate()
            .map(|(i, child)| {
                let obj = child
                    .as_object()
                    .ok_or_else(|| self.err(node, format!("`{key}[{i}]` is not an object")))?;
                Ok(Node {
                    obj,
                    path: format!("{}/{key}/{i}", node.path).into(),
                })
            })
            .collect()
    }

    /// The single child in list field `key`.
    pub(crate) fn only_child(&self, node: &Node<'a>, key: &str) -> Result<Node<'a>, Error> {
        let mut children = self.children(node, key)?;
        if children.len() != 1 {
            return Err(self.err(
                node,
                format!("expected exactly one `{key}`, found {}", children.len()),
            ));
        }
        Ok(children.remove(0))
    }

    /// The node that reference field `key` (an `addr`) points to.
    pub(crate) fn deref(&self, node: &Node<'a>, key: &str) -> Result<Node<'a>, Error> {
        let addr = self.str(node, key)?;
        self.nodes.get(addr).cloned().ok_or_else(|| {
            self.err(
                node,
                format!("`{key}` is `{addr}`, which names no node in this file"),
            )
        })
    }

    /// The node's `addr`.
    pub(crate) fn addr(&self, node: &Node<'a>) -> Result<&'a str, Error> {
        self.str(node, "addr")
    }

    /// Fails on any field outside [`COMMON_FIELDS`] and `known`. Empty child lists are
    /// accepted: they carry no information.
    pub(crate) fn check_fields(&self, node: &Node<'a>, known: &[&str]) -> Result<(), Error> {
        for (key, value) in node.obj {
            let empty_list = value.as_array().is_some_and(Vec::is_empty);
            if !(empty_list
                || COMMON_FIELDS.contains(&key.as_str())
                || known.contains(&key.as_str()))
            {
                return Err(self.err(
                    node,
                    format!(
                        "unknown field `{key}` on {}; this front end does not know what it means",
                        Index::kind(node)
                    ),
                ));
            }
        }
        Ok(())
    }
}

fn collect<'a>(value: &'a Value, path: &mut String, out: &mut BTreeMap<&'a str, Node<'a>>) {
    let len = path.len();
    match value {
        Value::Object(obj) => {
            if let Some(Value::String(addr)) = obj.get("addr") {
                out.insert(
                    addr,
                    Node {
                        obj,
                        path: path.as_str().into(),
                    },
                );
            }
            for (key, child) in obj {
                if child.is_array() || child.is_object() {
                    path.push('/');
                    path.push_str(key);
                    collect(child, path, out);
                    path.truncate(len);
                }
            }
        }
        Value::Array(list) => {
            for (i, child) in list.iter().enumerate() {
                path.push('/');
                path.push_str(&i.to_string());
                collect(child, path, out);
                path.truncate(len);
            }
        }
        _ => {}
    }
}
