use crate::{audit::{SectionId, SymbolId, Value, ValueBase}, elf::{Object, Symbol, SymbolSection}, Error};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(crate) struct Definition {
    pub id: SymbolId,
}

pub(crate) struct Resolver<'a> {
    pub objects: &'a [Object],
    pub globals: BTreeMap<String, Definition>,
}

impl<'a> Resolver<'a> {
    pub fn new(objects: &'a [Object]) -> Result<Self, Error> {
        let mut resolver = Self { objects, globals: BTreeMap::new() };
        for (object, obj) in objects.iter().enumerate() {
            for (&table, symbols) in &obj.symbols {
                for (index, symbol) in symbols.iter().enumerate() {
                    if symbol.bind == 0 || symbol.shndx == SymbolSection::Undef { continue; }
                    let id = SymbolId { object, table, index };
                    if let Some(previous) = resolver.globals.get(&symbol.name) {
                        let old = resolver.symbol(previous.id);
                        if old.bind == 1 && symbol.bind == 1 {
                            let old_at = resolver.definition_location(previous.id);
                            return Err(resolver.definition_location(id).error(format!("two strong definitions of {}: {old_at} and {}", symbol.name, resolver.definition_location(id))));
                        }
                        if old.bind == 1 || symbol.bind == 2 {
                            continue; // Equal weak definitions choose the first in input order.
                        }
                    }
                    resolver.globals.insert(symbol.name.clone(), Definition { id });
                }
            }
        }
        Ok(resolver)
    }

    // IDs are constructed only from parsed object/table indices inside the audit.
    pub fn symbol(&self, id: SymbolId) -> &Symbol { &self.objects[id.object].symbols[&id.table][id.index] }

    pub fn definition_location(&self, id: SymbolId) -> crate::Location {
        let symbol = self.symbol(id);
        match symbol.shndx {
            SymbolSection::Section(section) => self.objects[id.object].location(section).at(symbol.value),
            _ => self.objects[id.object].location(id.table).at((id.index * 16) as u32),
        }
    }

    pub fn definition(&self, id: SymbolId) -> Option<SymbolId> {
        let symbol = self.symbol(id);
        if symbol.bind != 0 {
            self.globals.get(&symbol.name).map(|d| d.id)
        } else if symbol.shndx != SymbolSection::Undef {
            Some(id)
        } else { None }
    }

    pub fn import(&self, id: SymbolId) -> Option<&Symbol> {
        let symbol = self.symbol(id);
        if symbol.shndx == SymbolSection::Undef && !symbol.name.is_empty() && self.definition(id).is_none() {
            Some(symbol)
        } else { None }
    }

    pub fn value(&self, id: SymbolId, extra: u32) -> Value {
        let original = self.symbol(id);
        let Some(definition) = self.definition(id) else {
            return Value {
                base: if original.name.is_empty() { ValueBase::Absolute } else { ValueBase::Import(original.name.clone()) },
                offset: extra,
            };
        };
        let symbol = self.symbol(definition);
        let (base, value) = match symbol.shndx {
            SymbolSection::Section(section) => (ValueBase::Section(SectionId { object: definition.object, section }), symbol.value),
            SymbolSection::Common => (ValueBase::Common(definition), 0), // st_value is alignment, not an address
            SymbolSection::Abs => (ValueBase::Absolute, symbol.value),
            SymbolSection::Undef => (ValueBase::Import(symbol.name.clone()), 0),
        };
        Value { base, offset: value.wrapping_add(extra) }
    }

    pub fn same_callee(&self, a: SymbolId, a_extra: u32, b: SymbolId, b_extra: u32) -> bool {
        let sa = self.symbol(a);
        let sb = self.symbol(b);
        if sa.kind != 3 && sb.kind != 3 && !sa.name.is_empty() && !sb.name.is_empty() {
            sa.name == sb.name && a_extra == b_extra
        } else {
            self.value(a, a_extra) == self.value(b, b_extra)
        }
    }
}
