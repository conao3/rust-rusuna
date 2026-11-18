use super::schema::{directives, insert, string, strings};
use super::*;
use crate::{Table, WriteKind, WritePermission};
use async_graphql_parser::Positioned;
use async_graphql_value::ConstValue;
use std::collections::BTreeSet;

type MutationRoots = BTreeMap<String, (String, WriteKind, bool, bool)>;

pub(super) fn definitions(
    tables: &[Table],
    fields: &BTreeMap<String, Vec<(String, String)>>,
    defs: &mut BTreeMap<String, String>,
    query: &str,
) -> Result<(MutationRoots, BTreeMap<String, String>), Error> {
    let mut roots = BTreeMap::new();
    let mut primary = BTreeMap::new();
    let mut mutation = String::from("type Mutation {\n");
    for table in tables {
        let t = &table.name;
        let columns = &fields[t];
        let entries = table
            .primary_key
            .as_ref()
            .map(|key| key_entries(&key.columns, columns))
            .transpose()?;
        if let Some(entries) = &entries {
            let name = format!("{t}_by_pk");
            let q = defs.get_mut(query).unwrap();
            if q.lines().any(|line| {
                line.trim_start().starts_with(&format!("{name}("))
                    || line.trim_start().starts_with(&format!("{name}:"))
            }) {
                return Err(error(
                    Pos::default(),
                    "primary key root collides with a declared field",
                ));
            }
            let end = q.rfind('}').unwrap();
            q.insert_str(end, &format!("  {name}({}): {t}\n", entries.join(", ")));
            primary.insert(name, t.clone());
        }
        if table.read_only {
            continue;
        }
        for suffix in ["insert_input", "set_input", "inc_input"] {
            let entries = columns
                .iter()
                .filter(|(_, ty)| suffix != "inc_input" || ["Int", "Float"].contains(&ty.as_str()))
                .map(|(f, ty)| format!("  {f}: {ty}\n"))
                .collect::<String>();
            insert(
                defs,
                format!("{t}_{suffix}"),
                if entries.is_empty() {
                    format!("input {t}_{suffix}\n")
                } else {
                    format!("input {t}_{suffix} {{\n{entries}}}\n")
                },
                Pos::default(),
            )?;
        }
        insert(
            defs,
            format!("{t}_mutation_response"),
            format!(
                "type {t}_mutation_response {{\n  affected_rows: Int!\n  returning: [{t}!]!\n}}\n"
            ),
            Pos::default(),
        )?;
        let conflict_arg = if let Some(key) = &table.primary_key {
            insert(
                defs,
                format!("{t}_constraint"),
                format!("enum {t}_constraint {{ {} }}\n", key.constraint),
                Pos::default(),
            )?;
            insert(
                defs,
                format!("{t}_update_column"),
                format!(
                    "enum {t}_update_column {{ {} }}\n",
                    columns
                        .iter()
                        .map(|(f, _)| f.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                Pos::default(),
            )?;
            insert(
                defs,
                format!("{t}_on_conflict"),
                format!(
                    "input {t}_on_conflict {{ constraint: {t}_constraint! update_columns: [{t}_update_column!]! where: {t}_bool_exp }}\n"
                ),
                Pos::default(),
            )?;
            format!(", on_conflict: {t}_on_conflict")
        } else {
            String::new()
        };
        let mut add = |name: String, kind, single, by_key, args: String| {
            mutation.push_str(&format!(
                "  {name}({args}): {}\n",
                if single {
                    t.clone()
                } else {
                    format!("{t}_mutation_response")
                }
            ));
            roots.insert(name, (t.clone(), kind, single, by_key));
        };
        add(
            format!("insert_{t}"),
            WriteKind::Insert,
            false,
            false,
            format!("objects: [{t}_insert_input!]!{conflict_arg}"),
        );
        add(
            format!("insert_{t}_one"),
            WriteKind::Insert,
            true,
            false,
            format!("object: {t}_insert_input!{conflict_arg}"),
        );
        add(
            format!("update_{t}"),
            WriteKind::Update,
            false,
            false,
            format!("where: {t}_bool_exp!, _set: {t}_set_input, _inc: {t}_inc_input"),
        );
        add(
            format!("delete_{t}"),
            WriteKind::Delete,
            false,
            false,
            format!("where: {t}_bool_exp!"),
        );
        if let Some(entries) = entries {
            insert(
                defs,
                format!("{t}_pk_columns_input"),
                format!("input {t}_pk_columns_input {{ {} }}\n", entries.join(" ")),
                Pos::default(),
            )?;
            add(
                format!("update_{t}_by_pk"),
                WriteKind::Update,
                true,
                true,
                format!(
                    "pk_columns: {t}_pk_columns_input!, _set: {t}_set_input, _inc: {t}_inc_input"
                ),
            );
            add(
                format!("delete_{t}_by_pk"),
                WriteKind::Delete,
                true,
                true,
                entries.join(", "),
            );
        }
    }
    mutation.push_str("}\n");
    if roots.is_empty() {
        mutation = "type Mutation\n".into();
    }
    insert(defs, "Mutation".into(), mutation, Pos::default())?;
    Ok((roots, primary))
}

fn key_entries(keys: &[String], columns: &[(String, String)]) -> Result<Vec<String>, Error> {
    if keys.is_empty()
        || keys.iter().collect::<BTreeSet<_>>().len() != keys.len()
        || keys.iter().any(|f| !columns.iter().any(|(c, _)| c == f))
    {
        return Err(error(
            Pos::default(),
            "primary key columns must be distinct scalar fields",
        ));
    }
    Ok(keys
        .iter()
        .map(|f| format!("{f}: {}!", columns.iter().find(|(c, _)| c == f).unwrap().1))
        .collect())
}

pub(super) fn permissions(
    objects: &BTreeMap<String, Positioned<TypeDefinition>>,
    tables: &mut [Table],
    document: &mut DocumentSchema,
) -> Result<(), Error> {
    for table in tables {
        let mut seen = BTreeSet::new();
        for d in &objects[&table.name].node.directives {
            let kind = match d.node.name.node.as_str() {
                "insert_permission" => WriteKind::Insert,
                "update_permission" => WriteKind::Update,
                "delete_permission" => WriteKind::Delete,
                _ => continue,
            };
            if table.read_only {
                return Err(error(d.pos, "a read-only table has no write permissions"));
            }
            directives(std::slice::from_ref(d), &[d.node.name.node.as_str()])?;
            let role = string(&d.node, "role")?;
            if role == "admin" || !seen.insert((role.clone(), d.node.name.node.to_string())) {
                return Err(error(
                    d.pos,
                    "write permissions require distinct non-admin roles per operation",
                ));
            }
            let columns = if kind == WriteKind::Delete {
                Vec::new()
            } else {
                strings(&d.node, "columns")?
            };
            if columns.iter().collect::<BTreeSet<_>>().len() != columns.len()
                || columns
                    .iter()
                    .any(|f| !table.columns.iter().any(|c| &c.field == f))
            {
                return Err(error(d.pos, "write columns must be distinct scalar fields"));
            }
            let mut parse =
                |key: &str, required: bool| -> Result<crate::Filter<crate::Operand>, Error> {
                    let Some(v) = d.node.get_argument(key) else {
                        return if required {
                            Err(error(d.pos, format!("{} requires {key}", d.node.name.node)))
                        } else {
                            Ok(crate::Filter::And(Vec::new()))
                        };
                    };
                    let (filter, types) =
                        query::permission_filter(document, &table.name, &v.node, v.pos)?;
                    document
                        .session_types
                        .extend(types.into_iter().map(|(name, ty)| (role.clone(), name, ty)));
                    Ok(filter)
                };
            let filter = parse("where", kind != WriteKind::Insert)?;
            let check = parse("check", kind != WriteKind::Delete)?;
            let mut presets = Vec::new();
            if let Some(value) = d.node.get_argument("set") {
                let ConstValue::Object(values) = &value.node else {
                    return Err(error(value.pos, "set requires an object"));
                };
                for (field, value) in values {
                    let (operand, ty) =
                        query::preset(document, &table.name, field.as_str(), value, d.pos)?;
                    if let crate::Operand::Session(name) = &operand {
                        document
                            .session_types
                            .push((role.clone(), name.clone(), ty));
                    }
                    presets.push((field.to_string(), operand));
                }
            }
            table.write_permissions.push(WritePermission {
                role,
                kind,
                columns,
                filter,
                check,
                presets,
            });
        }
    }
    Ok(())
}

pub(super) fn restrict(
    document: &mut DocumentSchema,
    schema: &crate::compiler::model::Schema,
    catalog: &crate::compiler::model::WriteCatalog,
    role: &str,
) {
    use crate::compiler::model;
    let visible = model::visible_schema(schema.clone(), role);
    document.primary_roots.retain(|_, target| {
        catalog
            .keys
            .iter()
            .find(|k| &k.target == target)
            .is_some_and(|k| {
                visible
                    .tables
                    .iter()
                    .find(|t| &t.name == target)
                    .is_some_and(|t| {
                        k.columns
                            .iter()
                            .all(|c| t.columns.iter().any(|f| &f.field == c))
                    })
            })
    });
    document
        .mutation_roots
        .retain(|_, (target, kind, single, _)| {
            let kind = match kind {
                WriteKind::Insert => model::WriteKind::Insert,
                WriteKind::Update => model::WriteKind::Update,
                WriteKind::Delete => model::WriteKind::Delete,
            };
            model::write_visible(catalog.clone(), role, target.clone(), kind)
                && (!*single || model::table_visible(schema.clone(), role, target.clone()))
        });
    if let Some(TypeKind::Object(object)) = document.types.get_mut("Mutation") {
        object.fields.retain(|f| {
            document
                .mutation_roots
                .contains_key(f.node.name.node.as_str())
        });
    }
    for table in &schema.tables {
        if !document
            .types
            .contains_key(&format!("{}_bool_exp", table.name))
            && document
                .mutation_roots
                .values()
                .any(|(t, _, _, _)| t == &table.name)
        {
            let source = format!(
                "input {}_bool_exp {{ _and: [{}_bool_exp!] _or: [{}_bool_exp!] _not: {}_bool_exp }}",
                table.name, table.name, table.name, table.name
            );
            let parsed =
                async_graphql_parser::parse_schema(&source).expect("generated Boolean input");
            for def in parsed.definitions {
                if let TypeSystemDefinition::Type(t) = def {
                    document
                        .types
                        .insert(t.node.name.node.to_string(), t.node.kind);
                }
            }
        }
        let update = model::find_write_permission(
            catalog.clone(),
            role,
            table.name.clone(),
            model::WriteKind::Update,
        );
        if let Some(TypeKind::Enum(enumeration)) = document
            .types
            .get_mut(&format!("{}_update_column", table.name))
        {
            enumeration.values.retain(|v| {
                update.as_ref().is_some_and(|p| {
                    p.columns.iter().any(|c| c == v.node.value.node.as_str())
                        && !p
                            .presets
                            .iter()
                            .any(|(c, _)| c == v.node.value.node.as_str())
                })
            });
        }
        if update.is_none()
            && let Some(TypeKind::Object(object)) = document.types.get_mut("Mutation")
        {
            for f in &mut object.fields {
                if document
                    .mutation_roots
                    .get(f.node.name.node.as_str())
                    .is_some_and(|(t, k, _, _)| t == &table.name && *k == WriteKind::Insert)
                {
                    f.node
                        .arguments
                        .retain(|a| a.node.name.node != "on_conflict");
                }
            }
        }

        for (suffix, kind) in [
            ("insert_input", model::WriteKind::Insert),
            ("set_input", model::WriteKind::Update),
            ("inc_input", model::WriteKind::Update),
        ] {
            let permission =
                model::find_write_permission(catalog.clone(), role, table.name.clone(), kind);
            if let Some(TypeKind::InputObject(input)) =
                document.types.get_mut(&format!("{}_{suffix}", table.name))
            {
                input.fields.retain(|f| {
                    permission.as_ref().is_some_and(|p| {
                        p.columns.iter().any(|c| c == f.node.name.node.as_str())
                            && !p
                                .presets
                                .iter()
                                .any(|(c, _)| c == f.node.name.node.as_str())
                    })
                });
            }
        }
    }
}
