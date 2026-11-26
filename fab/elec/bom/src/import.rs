//! Offline CSV preview. Column positions in a confirmation refer to this exact parsed file.
use crate::prices::{millionths, valid_date, Entry, Library, Source, Sourcing};
use std::collections::BTreeMap;

const ALIASES: &[(&str,&[&str])] = &[
    ("mpn",&["mpn","manufacturer part number","manufacturer part"]),
    ("supplier",&["supplier","vendor"]), ("supplier_part",&["supplier part","supplier part number","sku","vendor part number"]),
    ("price",&["price","unit price","unit cost","price each"]), ("currency",&["currency","currency code"]),
    ("min",&["min","break quantity","quantity","qty"]), ("moq",&["moq","minimum order","minimum order quantity"]),
    ("multiple",&["multiple","order multiple","pack size"]), ("assembly_class",&["assembly class"]),
    ("url",&["url","source url"]),
];

#[derive(Clone,Debug)]
pub struct Draft {
    pub header:Vec<String>,
    pub columns:Vec<String>,
    pub source:String,
    pub date:String,
    rows:Vec<Vec<String>>,
}

#[derive(Debug)]
pub struct Import {
    pub entries:Vec<Entry>,
    pub header:Vec<String>,
    pub mapping:Vec<String>,
}

fn sorted(header:&[String]) -> Vec<usize> {
    let mut order:Vec<_>=(0..header.len()).collect();
    order.sort_by_key(|&i| &header[i]);
    order
}

impl Draft {
    pub fn from_text(input:&str, filename:&str,date:&str, book:&Library) -> Result<Self,String> {
        if !valid_date(date) { return Err("invalid import date".into()); }
        let mut rows = fab_elec_cam::text::csv_rows(input.trim_start_matches('\u{feff}'),',').into_iter();
        let header = rows.next().filter(|r| r.iter().any(|v| !v.is_empty())).ok_or("the CSV needs a header")?;
        let order=sorted(&header);
        let key:Vec<_>=order.iter().map(|&i| header[i].clone()).collect();
        let mut columns:Vec<_>=header.iter().map(|h| {
            let normalized=h.trim().replace(['_','-']," ").to_ascii_lowercase();
            ALIASES.iter().find(|(_,a)| a.contains(&normalized.as_str())).map_or("-",|(k,_)| *k).to_owned()
        }).collect();
        if let Some(saved)=book.mappings.get(&key) {
            if saved.len()!=order.len() { return Err("invalid saved CSV mapping".into()); }
            for (&i,field) in order.iter().zip(saved) { columns[i].clone_from(field); }
        }
        let rows:Vec<_>=rows.filter(|r| r.iter().any(|v| !v.trim().is_empty())).collect();
        if rows.is_empty() { return Err("the CSV has no price rows".into()); }
        let source=std::path::Path::new(filename).file_name().and_then(|s| s.to_str()).filter(|n| !n.is_empty()).ok_or("missing CSV file name")?.into();
        Ok(Self { header,columns,source,date:date.into(),rows })
    }
    pub fn confirm(&self, columns:&[String]) -> Result<Import,String> {
        if columns.len()!=self.header.len() { return Err("map each header to one field or - to skip it".into()); }
        let mut indices=BTreeMap::new();
        for (i,k) in columns.iter().enumerate() {
            let k=k.trim();
            if k=="-" || k.is_empty() { continue; }
            if !ALIASES.iter().any(|(known,_)| *known==k) || indices.insert(k,i).is_some() {
                return Err(format!("unknown or repeated mapping: {k}"));
            }
        }
        for required in ["price","currency"] {
            if !indices.contains_key(required) { return Err(format!("map a {required} column")); }
        }
        if !indices.contains_key("mpn") && !(indices.contains_key("supplier") && indices.contains_key("supplier_part")) {
            return Err("map an MPN or both supplier and supplier part".into());
        }
        let mut entries=BTreeMap::new();
        for (i,row) in self.rows.iter().enumerate() {
            let read = || -> Result<(Entry,i64,i64),String> {
                if row.len()!=self.header.len() { return Err("column count differs from the header".into()); }
                let field=|k| indices.get(k).map_or("",|&j| row[j].trim());
                let part=Sourcing { mpn:field("mpn").into(),supplier:field("supplier").into(),supplier_part:field("supplier_part").into() };
                let key=part.supplier_key().or_else(|| part.mpn_key()).ok_or("missing purchase key")?;
                let currency=field("currency").to_string();
                if currency.len()!=3 || !currency.bytes().all(|c| c.is_ascii_uppercase()) { return Err("expected a three-letter currency".into()); }
                let count=|k| if field(k).is_empty() { Ok(1) } else { field(k).parse::<i64>().ok().filter(|n| *n>0).ok_or_else(|| format!("{k} must be positive")) };
                let optional=|k| (!field(k).is_empty()).then(|| field(k).to_owned());
                Ok((Entry { key,currency,moq:count("moq")?,multiple:count("multiple")?,breaks:BTreeMap::new(),
                    assembly_class:optional("assembly_class"),source:Source{name:self.source.clone(),date:self.date.clone(),url:optional("url")} },
                    count("min")?,millionths(field("price"))?))
            };
            let (e,min,micros)=read().map_err(|why| format!("CSV row {}: {why}",i+2))?;
            let stored=entries.entry(e.key.clone()).or_insert_with(|| e.clone());
            if stored.currency!=e.currency || stored.moq!=e.moq || stored.multiple!=e.multiple || stored.assembly_class!=e.assembly_class || stored.source!=e.source ||
                stored.breaks.insert(min,micros).is_some_and(|p| p!=micros) {
                return Err(format!("CSV row {} conflicts with another entry for this part",i+2));
            }
        }
        let order=sorted(&self.header);
        Ok(Import { entries:entries.into_values().collect(),header:order.iter().map(|&i| self.header[i].clone()).collect(),
            mapping:order.iter().map(|&i| columns[i].trim().into()).collect() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirmed_header_mapping_follows_columns_and_stamps_the_receipt() {
        let mut book=Library::default();
        let draft=Draft::from_text("Manufacturer Part Number,My cost,Currency,Quantity\r\n\"A,1\",0.80,USD,1\r\n\"A,1\",0.50,USD,10","/tmp/receipt.csv","2026-11-25",&book).unwrap();
        assert_eq!(draft.columns,["mpn","-","currency","min"]);
        assert!(draft.confirm(&draft.columns).is_err());
        let confirmed=draft.confirm(&["mpn","price","currency","min"].map(String::from)).unwrap();
        assert_eq!(confirmed.entries[0].breaks,[(1,800000),(10,500000)].into());
        assert_eq!((&*confirmed.entries[0].source.name,&*confirmed.entries[0].source.date),("receipt.csv","2026-11-25"));
        book.mappings.insert(confirmed.header,confirmed.mapping);
        let again=Draft::from_text("Currency,My cost,Quantity,Manufacturer Part Number\nEUR,1.50,2,B","cart.csv","2026-11-26",&book).unwrap();
        assert_eq!(again.columns,["currency","price","min","mpn"]);
        assert_eq!(again.confirm(&again.columns).unwrap().entries[0].breaks[&2],1_500_000);
    }
}
