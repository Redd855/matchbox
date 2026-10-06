//! Module members retain their callable, independent of replaceable global aliases.
use super::{
    VM,
    gc::{GcObject, RootedValue},
};
use crate::{
    bundle::BifMember,
    types::{BxVM, BxValue},
};
use anyhow::{Context, Result, bail};
use std::rc::Rc;

#[derive(Clone)]
pub(super) struct RegisteredMember {
    pub function: Rc<RootedValue>,
    pub parameters: Vec<String>,
    pub object_index: usize,
}
impl RegisteredMember {
    pub fn arguments(
        &self,
        receiver: BxValue,
        args: Vec<BxValue>,
        names: Option<Vec<String>>,
    ) -> Result<Vec<BxValue>> {
        let mut values = vec![BxValue::new_null(); self.parameters.len()];
        let mut assigned = vec![false; values.len()];
        values[self.object_index] = receiver;
        assigned[self.object_index] = true;
        let mut positional = 0;
        for (index, value) in args.into_iter().enumerate() {
            let name = names
                .as_ref()
                .and_then(|names| names.get(index))
                .filter(|name| !name.is_empty());
            let slot = if let Some(name) = name {
                let slot = self
                    .parameters
                    .iter()
                    .position(|parameter| parameter.eq_ignore_ascii_case(name))
                    .with_context(|| format!("Unknown member argument: {name}"))?;
                // JVM member dispatch always supplies the actual receiver, even if a caller names it.
                if slot == self.object_index {
                    continue;
                }
                slot
            } else {
                while positional == self.object_index {
                    positional += 1;
                }
                let slot = positional;
                positional += 1;
                if slot >= values.len() {
                    bail!("Too many member arguments");
                }
                slot
            };
            if assigned[slot] {
                bail!("Duplicate member argument: {}", self.parameters[slot]);
            }
            assigned[slot] = true;
            values[slot] = value;
        }
        Ok(values)
    }
}
impl VM {
    pub fn register_member_method(&mut self, function: BxValue, member: &BifMember) -> Result<()> {
        let type_name = member.type_name.to_lowercase();
        if !matches!(
            type_name.as_str(),
            "any"
                | "array"
                | "boolean"
                | "class"
                | "closure"
                | "date"
                | "datetime"
                | "double"
                | "function"
                | "integer"
                | "lambda"
                | "list"
                | "long"
                | "numeric"
                | "string"
                | "string_strict"
                | "struct"
                | "udf"
                | "modifiablearray"
                | "assignablearray"
                | "modifiablestruct"
                | "structloose"
        ) {
            bail!(
                "Invalid or unsupported BoxMember type: {}",
                member.type_name
            );
        }
        let parameters = match function.as_gc_id().and_then(|id| self.heap.get_opt(id)) {
            Some(GcObject::CompiledFunction(function)) => function.params.clone(),
            Some(GcObject::NativeCallback(function)) => function.parameters.clone(),
            _ => bail!("Module member must refer to a callable with declared arguments"),
        };
        let object_index = if let Some(name) = &member.object_argument {
            parameters
                .iter()
                .position(|parameter| parameter.eq_ignore_ascii_case(name))
                .with_context(|| {
                    format!("BoxMember objectArgument does not name an invoke() parameter: {name}")
                })?
        } else {
            0
        };
        if parameters.is_empty() {
            bail!("BoxMember invoke() must accept a receiver argument");
        }
        let function = self
            .heap
            .retain_value(function)
            .map_err(anyhow::Error::msg)?;
        self.members
            .entry(member.name.to_lowercase())
            .or_default()
            .insert(
                type_name,
                RegisteredMember {
                    function,
                    parameters,
                    object_index,
                },
            );
        Ok(())
    }

    pub(super) fn registered_member(
        &mut self,
        receiver: BxValue,
        name: &str,
    ) -> Option<(RegisteredMember, BxValue)> {
        if self.members.is_empty() || receiver.is_null() {
            return None;
        }
        let name = name.to_lowercase();
        let methods = self.members.get(&name)?;
        if let Some(object) = receiver.as_gc_id().and_then(|id| self.heap.get_opt(id)) {
            match object {
                GcObject::NativeObject(object)
                    if object.borrow().delegated_callback(&name).is_some() =>
                {
                    return None;
                }
                GcObject::Instance(instance)
                    if self.resolve_method(instance.class.clone(), &name).is_some() =>
                {
                    return None;
                }
                _ => (),
            }
        }
        // Prefer the receiver's actual type over the generic any registration.
        for type_name in [
            "string_strict",
            "string",
            "array",
            "modifiablearray",
            "assignablearray",
            "struct",
            "modifiablestruct",
            "structloose",
            "boolean",
            "integer",
            "long",
            "numeric",
            "double",
            "datetime",
            "date",
            "class",
            "closure",
            "lambda",
            "udf",
            "function",
            "list",
        ] {
            let Some(member) = methods.get(type_name) else {
                continue;
            };
            let base_type = match type_name {
                "string_strict" | "list" => "string",
                "modifiablearray" | "assignablearray" => "array",
                "modifiablestruct" | "structloose" => "struct",
                "date" => "datetime",
                _ => type_name,
            };
            if self.value_matches_type_name(receiver, base_type) {
                return Some((member.clone(), receiver));
            }
        }
        for type_name in [
            "integer", "long", "numeric", "double", "boolean", "string", "list", "any",
        ] {
            let Some(member) = self.members.get(&name)?.get(type_name).cloned() else {
                continue;
            };
            let scalar =
                receiver.is_number() || receiver.is_bool() || self.is_string_value(receiver);
            if type_name != "any" && !scalar {
                continue;
            }
            if type_name == "boolean" && self.is_string_value(receiver) {
                let text = self.to_string(receiver);
                if !matches!(
                    text.trim().to_lowercase().as_str(),
                    "true" | "false" | "yes" | "no"
                ) && text.trim().parse::<f64>().is_err()
                {
                    continue;
                }
            }
            if let Ok(receiver) = self.cast_value_to_type(
                receiver,
                if type_name == "list" {
                    "string"
                } else {
                    type_name
                },
            ) {
                return Some((member, receiver));
            }
        }
        None
    }
}
