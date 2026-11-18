mod support;

use async_graphql::dynamic::{Field, FieldFuture, TypeRef};
use async_graphql::parser::types::{BaseType, Type, TypeKind, TypeSystemDefinition};
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use support::fixture;

type Fields = Vec<(String, Vec<(String, String)>, String)>;

#[derive(Debug, PartialEq)]
enum Shape {
    Object(Fields),
    Input(Vec<(String, String)>),
    Enum(Vec<String>),
    Scalar,
}

fn written(ty: &Type) -> String {
    let base = match &ty.base {
        BaseType::Named(name) => name.to_string(),
        BaseType::List(inner) => format!("[{}]", written(inner)),
    };
    if ty.nullable {
        base
    } else {
        format!("{base}!")
    }
}

fn from_sdl(sdl: &str) -> (String, Option<String>, BTreeMap<String, Shape>) {
    let document = async_graphql::parser::parse_schema(sdl).unwrap();
    let mut roots = ("Query".to_string(), None);
    let mut types = BTreeMap::new();
    for definition in document.definitions {
        match definition {
            TypeSystemDefinition::Schema(schema) => {
                roots = (
                    schema.node.query.unwrap().node.to_string(),
                    schema.node.mutation.map(|m| m.node.to_string()),
                );
            }
            TypeSystemDefinition::Type(ty) => {
                let shape = match ty.node.kind {
                    TypeKind::Object(object) => Shape::Object(
                        object
                            .fields
                            .into_iter()
                            .map(|f| {
                                (
                                    f.node.name.node.to_string(),
                                    f.node
                                        .arguments
                                        .iter()
                                        .map(|a| {
                                            (a.node.name.node.to_string(), written(&a.node.ty.node))
                                        })
                                        .collect(),
                                    written(&f.node.ty.node),
                                )
                            })
                            .collect(),
                    ),
                    TypeKind::InputObject(input) => Shape::Input(
                        input
                            .fields
                            .iter()
                            .map(|f| (f.node.name.node.to_string(), written(&f.node.ty.node)))
                            .collect(),
                    ),
                    TypeKind::Enum(e) => Shape::Enum(
                        e.values
                            .iter()
                            .map(|v| v.node.value.node.to_string())
                            .collect(),
                    ),
                    TypeKind::Scalar => Shape::Scalar,
                    other => panic!("unexpected {other:?}"),
                };
                types.insert(ty.node.name.node.to_string(), shape);
            }
            TypeSystemDefinition::Directive(_) => unreachable!(),
        }
    }
    (roots.0, roots.1, types)
}

fn introspected(ty: &Json) -> String {
    match ty["kind"].as_str().unwrap() {
        "NON_NULL" => format!("{}!", introspected(&ty["ofType"])),
        "LIST" => format!("[{}]", introspected(&ty["ofType"])),
        _ => ty["name"].as_str().unwrap().to_string(),
    }
}

const QUERY: &str = "{ __schema { queryType { name } mutationType { name } types { kind name fields { name args { name type { ...T } } type { ...T } } inputFields { name type { ...T } } enumValues { name } } } } fragment T on __Type { kind name ofType { kind name ofType { kind name ofType { kind name } } } }";

fn from_introspection(data: &Json) -> (String, Option<String>, BTreeMap<String, Shape>) {
    let schema = &data["__schema"];
    let pairs = |values: &Json| -> Vec<(String, String)> {
        values
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                (
                    v["name"].as_str().unwrap().to_string(),
                    introspected(&v["type"]),
                )
            })
            .collect()
    };
    let mut types = BTreeMap::new();
    for ty in schema["types"].as_array().unwrap() {
        let name = ty["name"].as_str().unwrap();
        if name.starts_with("__")
            || ["Int", "Float", "String", "Boolean", "ID", "Viewer"].contains(&name)
        {
            continue;
        }
        let shape = match ty["kind"].as_str().unwrap() {
            "OBJECT" => Shape::Object(
                ty["fields"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|f| f["name"] != "viewer")
                    .map(|f| {
                        (
                            f["name"].as_str().unwrap().to_string(),
                            pairs(&f["args"]),
                            introspected(&f["type"]),
                        )
                    })
                    .collect(),
            ),
            "INPUT_OBJECT" => Shape::Input(pairs(&ty["inputFields"])),
            "ENUM" => Shape::Enum(
                ty["enumValues"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v["name"].as_str().unwrap().to_string())
                    .collect(),
            ),
            "SCALAR" => Shape::Scalar,
            other => panic!("unexpected {other}"),
        };
        types.insert(name.to_string(), shape);
    }
    (
        schema["queryType"]["name"].as_str().unwrap().to_string(),
        schema["mutationType"]["name"].as_str().map(str::to_string),
        types,
    )
}

fn reachable(query: &str, mutation: Option<&str>, types: &mut BTreeMap<String, Shape>) {
    let mut seen = std::collections::BTreeSet::new();
    let mut pending: Vec<String> = std::iter::once(query)
        .chain(mutation)
        .map(str::to_string)
        .collect();
    while let Some(name) = pending.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let named = |ty: &str| {
            ty.trim_matches(|c| c == '[' || c == ']' || c == '!')
                .to_string()
        };
        match types.get(&name) {
            Some(Shape::Object(fields)) => {
                for (_, arguments, ty) in fields {
                    pending.push(named(ty));
                    pending.extend(arguments.iter().map(|(_, ty)| named(ty)));
                }
            }
            Some(Shape::Input(fields)) => pending.extend(fields.iter().map(|(_, ty)| named(ty))),
            _ => {}
        }
    }
    types.retain(|name, _| seen.contains(name));
}

async fn introspect(schema: &Arc<rusuna::Schema>, role: &str) -> Json {
    let built = rusuna_async_graphql::Builder::new(schema.clone(), role)
        .build()
        .unwrap()
        .query_field(Field::new("viewer", TypeRef::named(TypeRef::INT), |_| {
            FieldFuture::new(async { Ok(None::<async_graphql::Value>) })
        }))
        .schema()
        .finish()
        .unwrap();
    let response = built.execute(QUERY).await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    response.data.into_json().unwrap()
}

#[tokio::test]
async fn introspection_equals_the_expanded_sdl_of_each_role() {
    let mut compared = 0;
    for (path, pruned) in [
        ("schema.graphql", json!({"visitor": ["Query", "Mutation"]})),
        (
            "permissions.graphql",
            json!({"user": ["Mutation"], "reviewer": ["Mutation"], "visitor": ["Query", "Mutation"]}),
        ),
        (
            "mutations/schema.graphql",
            json!({
                "static": ["Query", "mutation_item_inc_input", "update_mutation_item._inc"],
                "writer": ["Query", "mutation_item_inc_input", "update_mutation_item._inc"],
                "visitor": ["Query", "Mutation"],
            }),
        ),
    ] {
        let schema = Arc::new(rusuna::Schema::from_sdl(&fixture(path)).unwrap());
        let mut roles = schema.roles();
        roles.push("visitor".into());
        for role in roles {
            let (query, mutation, mut types) = from_sdl(&schema.expanded_sdl(&role).unwrap());
            let mut mutation = mutation;
            for item in pruned[&role].as_array().into_iter().flatten() {
                let item = item.as_str().unwrap();
                if let Some((field, argument)) = item.split_once('.') {
                    for shape in types.values_mut() {
                        if let Shape::Object(fields) = shape {
                            for (name, arguments, _) in fields.iter_mut() {
                                if name == field {
                                    arguments.retain(|(a, _)| a != argument);
                                }
                            }
                        }
                    }
                } else if item == "Query" {
                    types.insert(item.to_string(), Shape::Object(Vec::new()));
                } else {
                    types.remove(item);
                    if mutation.as_deref() == Some(item) {
                        mutation = None;
                    }
                }
            }
            reachable(&query, mutation.as_deref(), &mut types);
            let data = introspect(&schema, &role).await;
            assert_eq!(
                from_introspection(&data),
                (query, mutation, types),
                "{path} {role}"
            );
            compared += 1;
        }
    }
    assert_eq!(compared, 12);
}

#[test]
fn hand_written_fields_name_declared_row_types_and_scalar_fields() {
    let schema = Arc::new(rusuna::Schema::from_sdl(&fixture("permissions.graphql")).unwrap());
    let build = |type_name: &str, requires: &[&str]| {
        rusuna_async_graphql::Builder::new(schema.clone(), "reviewer")
            .field(
                type_name,
                "extra",
                TypeRef::named(TypeRef::INT),
                requires,
                |_| FieldFuture::new(async { Ok(None::<async_graphql::Value>) }),
            )
            .build()
            .map(|_| ())
    };
    assert!(build("articel", &[]).is_err());
    assert!(build("article", &["titel"]).is_err());
    assert!(build("article", &["author"]).is_err());
    assert!(build("article", &["published_at"]).is_ok());
    assert!(build("revision", &[]).is_ok());
}
