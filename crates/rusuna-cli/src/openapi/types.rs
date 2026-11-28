use super::{
    Json, Map, column, mapped_name, properties, required, resolve, type_name, unique, valid_name,
};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(super) enum Direction {
    Input,
    Output,
}

pub(super) struct Types<'a> {
    document: &'a Json,
    components: HashMap<*const Json, &'a str>,
    names: BTreeSet<String>,
    generated: HashMap<(*const Json, Direction), String>,
    definitions: Vec<String>,
    json: bool,
    date_time: bool,
}

pub(super) fn schema_type(schema: &Json) -> Option<&str> {
    if schema.get("oneOf").is_some() || schema.get("anyOf").is_some() {
        return None;
    }
    match &schema["type"] {
        Json::String(kind) => Some(kind),
        Json::Array(kinds) => {
            let mut kinds = kinds
                .iter()
                .filter_map(Json::as_str)
                .filter(|kind| *kind != "null");
            let kind = kinds.next()?;
            kinds.next().is_none().then_some(kind)
        }
        _ => None,
    }
}

pub(super) fn object_properties(schema: &Json) -> Option<&Map<String, Json>> {
    if schema_type(schema) != Some("object") {
        return None;
    }
    schema["properties"]
        .as_object()
        .filter(|fields| !fields.is_empty())
}

fn permits_null(schema: &Json) -> bool {
    schema["nullable"] == true
        || schema["type"] == "null"
        || schema["type"]
            .as_array()
            .is_some_and(|types| types.iter().any(|kind| kind == "null"))
}

enum StringEnum<'a> {
    Values(Vec<&'a str>),
    String,
}

fn string_enum(schema: &Json) -> Option<StringEnum<'_>> {
    let values = schema["enum"].as_array()?;
    if values.is_empty()
        || !matches!(schema_type(schema), Some("string") | None)
        || schema.get("oneOf").is_some()
        || schema.get("anyOf").is_some()
    {
        return None;
    }
    let mut strings = BTreeSet::new();
    let mut names = Vec::new();
    for value in values.iter().filter(|value| !value.is_null()) {
        let name = value.as_str()?;
        if !valid_name(name) || matches!(name, "true" | "false" | "null") {
            return Some(StringEnum::String);
        }
        if strings.insert(name) {
            names.push(name);
        }
    }
    (!names.is_empty()).then_some(StringEnum::Values(names))
}

impl<'a> Types<'a> {
    pub(super) fn new(document: &'a Json) -> Self {
        Self {
            document,
            components: properties(&document["components"]["schemas"])
                .map(|(name, schema)| (std::ptr::from_ref(schema), name))
                .collect(),
            names: [
                "Query",
                "Mutation",
                "Subscription",
                "String",
                "Int",
                "Float",
                "Boolean",
                "ID",
                "DateTime",
                "JSON",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            generated: HashMap::new(),
            definitions: Vec::new(),
            json: false,
            date_time: false,
        }
    }

    pub(super) fn convert(
        &mut self,
        schema: &'a Json,
        hint: &str,
        direction: Direction,
        required: bool,
    ) -> Result<String, String> {
        self.convert_at_depth(schema, hint, direction, required, 0)
    }

    fn convert_at_depth(
        &mut self,
        schema: &'a Json,
        hint: &str,
        direction: Direction,
        required: bool,
        depth: usize,
    ) -> Result<String, String> {
        if depth > 64 {
            return Err("schema nesting exceeds 64 levels".into());
        }
        let schema = resolve(self.document, schema)?;
        let nullable = permits_null(schema);
        let mut ty = match string_enum(schema) {
            Some(StringEnum::Values(values)) => self.enumeration(schema, hint, &values),
            Some(StringEnum::String) => "String".into(),
            None => match schema_type(schema) {
                Some("integer") => "Int".into(),
                Some("number") => "Float".into(),
                Some("boolean") => "Boolean".into(),
                Some("string") if schema["format"] == "date-time" => {
                    self.date_time = true;
                    "DateTime".into()
                }
                Some("string") => "String".into(),
                Some("array") => {
                    let item = self.convert_at_depth(
                        &schema["items"],
                        &format!("{hint}Item"),
                        direction,
                        true,
                        depth + 1,
                    )?;
                    format!("[{item}]")
                }
                Some("object") => match object_properties(schema) {
                    Some(fields) => self.object(schema, hint, direction, fields, depth + 1)?,
                    _ => self.json(),
                },
                _ => self.json(),
            },
        };
        if required && !nullable {
            ty.push('!');
        }
        Ok(ty)
    }

    fn json(&mut self) -> String {
        self.json = true;
        "JSON".into()
    }

    fn name(&mut self, schema: &'a Json, hint: &str, suffix: &str) -> String {
        let name = match self.components.get(&std::ptr::from_ref(schema)) {
            Some(&name) if valid_name(name) => name.to_owned(),
            Some(&name) => type_name(name),
            None => hint.to_owned(),
        };
        unique(&mut self.names, format!("{name}{suffix}"))
    }

    fn enumeration(&mut self, schema: &'a Json, hint: &str, values: &[&str]) -> String {
        let key = (std::ptr::from_ref(schema), Direction::Output);
        if let Some(name) = self.generated.get(&key) {
            return name.clone();
        }
        let suffix = if self.components.contains_key(&key.0) {
            ""
        } else {
            "Enum"
        };
        let name = self.name(schema, hint, suffix);
        self.generated.insert(key, name.clone());
        self.definitions
            .push(format!("enum {name} {{\n  {}\n}}\n", values.join("\n  ")));
        name
    }

    fn object(
        &mut self,
        schema: &'a Json,
        hint: &str,
        direction: Direction,
        fields: &'a Map<String, Json>,
        depth: usize,
    ) -> Result<String, String> {
        let key = (std::ptr::from_ref(schema), direction);
        if let Some(name) = self.generated.get(&key) {
            return Ok(name.clone());
        }
        let (kind, suffix) = match direction {
            Direction::Input => ("input", "Input"),
            Direction::Output => ("type", ""),
        };
        let name = self.name(schema, hint, suffix);
        self.generated.insert(key, name.clone());
        let mut definition = format!("{kind} {name} {{\n");
        let mut names = fields
            .keys()
            .filter(|name| valid_name(name))
            .cloned()
            .collect();
        for (property, field) in fields {
            let field_name = mapped_name(property, &mut names);
            let ty = self.convert_at_depth(
                field,
                &format!("{name}{}", type_name(&field_name)),
                direction,
                required(schema, property),
                depth,
            )?;
            write!(
                definition,
                "  {field_name}: {ty}{}",
                column(&field_name, property)
            )
            .unwrap();
            definition.push('\n');
        }
        definition.push_str("}\n");
        self.definitions.push(definition);
        Ok(name)
    }

    pub(super) fn render(self, sdl: &mut String) {
        if self.json {
            sdl.push_str("\nscalar JSON\n");
        }
        if self.date_time {
            sdl.push_str("\nscalar DateTime\n");
        }
        for definition in self.definitions {
            sdl.push('\n');
            sdl.push_str(&definition);
        }
    }
}
