//! The sentence a view shows for a refused edit. The host keeps the diagnostic alongside it.

/// Describe a refusal once on the engine strand. Time and temporary space are linear in the
/// diagnostic's length. A name can be missing here without the language knowing where it belongs.
pub(crate) fn reason(text: &str) -> String {
    let text = text.trim();
    let text = text.split_once("Did you mean:").map_or(text, |(before, _)| before);
    // Verification can prefix the underlying error with the bench and the control being edited.
    let text = text.split_once("splice verification failed: ").map_or(text, |(_, why)| why);
    let text = without_position(text.trim());
    let text = text.lines().next().unwrap_or_default();
    let text = without_ids(text);
    let end = text.char_indices().find(|&(i, c)| matches!(c, '.' | '?' | '!')
        && text[i + c.len_utf8()..].starts_with(char::is_whitespace));
    let first = end.map_or(text.as_str(), |(i, c)| &text[..i + c.len_utf8()]).trim();
    let rewritten = edit_reason(first.trim_end_matches('.'));
    let message = rewritten.as_deref().unwrap_or(first);
    let mut chars = message.chars();
    let Some(initial) = chars.next().filter(|_| message.chars().any(char::is_alphanumeric)) else {
        return "The edit could not be applied.".into();
    };
    let mut sentence: String = initial.to_uppercase().collect();
    sentence.push_str(chars.as_str());
    if !sentence.ends_with(['.', '?', '!']) { sentence.push('.'); }
    sentence
}

fn without_position(text: &str) -> &str {
    // Read line and column from the end of the prefix, even when the path contains a colon.
    let (site, rest) = text.split_once(": ").unwrap_or((text.trim_end_matches(':'), ""));
    let mut fields = site.rsplitn(3, ':');
    match (fields.next(), fields.next(), fields.next()) {
        (Some(column), Some(line), Some(_)) if digits(line) && digits(column) => rest.trim_start(),
        _ => text,
    }
}

fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

// A VM value can carry an annotation even outside its list of suggestions. Keep ordinary
// parentheses, including component names and numeric values that occur in design messages.
fn without_ids(text: &str) -> String {
    let mut clean = String::new();
    let mut start = 0;
    for (at, _) in text.match_indices('(') {
        let tail = &text[at + 1..];
        let Some(number) = tail.strip_prefix("obj").or_else(|| tail.strip_prefix("fn"))
            .or_else(|| tail.strip_prefix("proto=obj")) else { continue };
        let len = number.bytes().take_while(u8::is_ascii_digit).count();
        if len > 0 && number[len..].starts_with(')') {
            clean.push_str(&text[start..at]);
            let rest = &number[len + 1..];
            let rest = if clean.ends_with(char::is_whitespace) { rest.trim_start() } else { rest };
            start = text.len() - rest.len();
        }
    }
    clean.push_str(&text[start..]);
    clean
}

fn edit_reason(message: &str) -> Option<String> {
    if let Some(name) = message.strip_prefix("variable ").and_then(|s| s.strip_suffix(" not found in scope")) {
        return Some(format!("This edit uses the name {name}, which is unavailable here"));
    }
    if let Some(name) = message.strip_prefix("property ").and_then(|s| s.strip_suffix(" not found in prototype chain")) {
        return Some(format!("This edit refers to a missing field named {name}"));
    }
    if let Some((net, block)) = message.strip_suffix(": give the block a port").and_then(|s| s.split_once(" is not reachable inside ")) {
        return Some(format!("Give the block a port to reach net {net} from {block}"));
    }
    if message.starts_with("shared net ") && message.ends_with(" is not in scope at this statement") {
        // End::Shared carries a paste-local number, not a net name.
        return Some("These parts cannot all reach the shared net here".into());
    }
    if let Some(pins) = message.strip_prefix("no scope names both pins: ").and_then(|s| s.strip_suffix(" (give the block a port)")) {
        return Some(format!("Give the block a port so you can connect {pins}"));
    }
    if let Some(count) = message.strip_prefix("written in a body that ").and_then(|s| s.strip_suffix(" components share: give the block a port or a parameter")) {
        return Some(format!("This change would affect {count} parts, so give the block a port or a parameter to change one"));
    }
    if let Some((pin, sites)) = message.split_once(" is connected in ") {
        if let Some((count, _)) = sites.split_once(" places: ") {
            return Some(format!("Edit the connections for {pin} in the source because it is connected in {count} places"));
        }
    }
    if let Some(pin) = message.strip_suffix(" is not reachable here: bind the part to a name") {
        return Some(format!("Give the part a name in the source to reach {pin} here"));
    }
    if let Some((part, _)) = message.split_once(": the value is computed by `") {
        return Some(format!("Change the expression in the source that sets the value of {part}"));
    }
    if let Some(control) = message.strip_suffix(": this value is computed here; edit the values it is made from") {
        return Some(format!("Change the inputs that determine {control} in the bench source"));
    }
    if let Some(name) = message.strip_suffix(" is already bound in this scope") {
        return Some(format!("Choose another name because {name} is already in use here"));
    }
    if let Some(name) = message.strip_prefix("renaming to ").and_then(|s| s.strip_suffix(" would capture this use in another scope")) {
        return Some(format!("Choose another name because {name} would refer to a different item"));
    }
    if let Some(name) = message.strip_suffix(" is not a binding identifier") {
        return Some(format!("Choose a valid name in place of {name}"));
    }
    if let Some(pin) = message.strip_suffix(" belongs to a generated part: only e.part, e.r, e.c and e.led pins can be no-connect") {
        return Some(format!("The sheet cannot mark {pin} as unconnected because the source generates this part"));
    }
    if let Some(field) = message.strip_suffix(": only e.part, e.r, e.c and e.led calls write a component's fields")
        .or_else(|| message.strip_suffix(": the options are not a literal object: edit them by hand")) {
        return Some(format!("Change {field} in the part's source definition"));
    }
    if let Some((why, action)) = message.strip_suffix(" by hand").and_then(|s| s.rsplit_once(": ")) {
        let why = match why {
            "the component's call is inside another expression" => "the part is defined inside a larger expression",
            "the component's call has no enclosing statement" => "the part has no separate definition",
            "this item is not the value of a simple let binding" => "the source does not name this item directly",
            "the pins need overlapping text edits" | "the moved pins need overlapping text edits" => "the pins share part of their source definition",
            "the sheet table and circuit operation need overlapping edits" => "the layout and circuit changes affect the same source text",
            "the part's nc list is not a literal list" => "the source computes the list of unconnected pins",
            "the pin's nc entry is not a literal name" => "the source computes the name of the unconnected pin",
            "the e.connect call is not a statement by itself" => "the connection is part of a larger expression",
            why => why,
        };
        return Some(format!("{action} in the source because {why}"));
    }
    let reason = match message {
        "the editor read an earlier load of the documents; it reads them again"
        | "the document changed since the editor read it; it reads it again" => "The document changed before this edit could be applied",
        "the sheet table is written by the editor: write it as literal entries" => "Write the sheet layout as individual entries so the editor can change them",
        "a circuit document may have only one sheet call" => "Keep the sheet layout in a single place in the circuit source",
        "a sheet hint needs the new part to have a name (write let <stem> = ..)" => "Give the new part a name in the source so the sheet can store its position",
        "two ops of one commit edit the same text: commit them one at a time" => "Make these changes one at a time because they affect the same source text",
        "the pins to move are written in different blocks: move them one block at a time" => "Move the pins one block at a time because they belong to different blocks",
        "this call is in a library, which is not edited here" => "This item is defined in a library that is read-only here",
        "ref: expected nonempty text without whitespace" => "Enter a reference with at least one character and no spaces",
        "this call makes several components: edit their references in the source" => "Change the references in the source because one definition creates several parts",
        "only e.part, e.r, e.c and e.led calls write a component's reference"
        | "the component's options are not a literal object: edit its reference in the source" => "Change the reference in the source that defines this part",
        "the pin's mapped connection expression is missing" | "the pin's connection has no enclosing statement" => "The editor cannot find this pin's connection in the source",
        "the component has no mapped call" | "the component's mapped call is missing" => "The editor cannot find this part's definition in the source",
        "the item has no mapped call" | "the mapped call is outside its document" => "The editor cannot find this item's definition in the source",
        "bind the component to a name first" => "Give the part a name in the source before connecting it",
        "removing a pin from nc needs a literal list" => "Change the list of unconnected pins in the source",
        "adding a pin needs a pins object: pins is not an object" => "List the part's pin connections individually in the source to add a pin here",
        "the value and tolerance need overlapping edits: edit the expression" => "Change the value and tolerance together in the expression that sets them",
        _ if message.starts_with("the tolerance is computed by `") => "Change the expression in the source that sets the tolerance",
        _ if message.starts_with("new statement at ") || message.starts_with("new statement: expected exactly one statement") => "This edit needs one valid circuit statement in the source",
        _ if message.starts_with("new statement must contain ") || message.starts_with("the new statement has no _n") => "The part definition needs a place for each pin connection",
        _ => return model_reason(message).map(str::to_owned),
    };
    Some(reason.into())
}

// The model returns DesignOpError directly. The board splicer wraps a board error with the op.
fn model_reason(message: &str) -> Option<&'static str> {
    let error = message.strip_prefix("Board(").and_then(|s| s.strip_suffix(')'))
        .or_else(|| message.strip_prefix("board op ").and_then(|s| s.split_once("): ")).map(|(_, error)| error))
        .unwrap_or(message);
    Some(match error.split(['(', '{']).next()?.trim_end() {
        "NoItem" => "This item is missing from the board",
        "DuplicateId" => "The board already contains an item with this identity",
        "KindMismatch" => "Choose a replacement of the same kind as the board item",
        "BlockItem" => "This item moves only with its block",
        "Derived" => "Change the constraint that controls this board item",
        "NotDetachable" => "This item has no block to detach from",
        "BadField" => "This board item does not support that setting or value",
        "BadAnchor" => "The track end needs an existing connection point",
        "BadPosition" => "This board item cannot be inserted at that position",
        "NoUnit" => "The selected unit is unavailable for this edit",
        "NoLink" => "The link is missing from the design",
        "DuplicateLink" => "The design already contains a link with this identity",
        "LinkInUse" => "A unit is mounted on this link",
        "BadMountParent" => "Choose an existing unit or link other than the unit being mounted",
        "CircuitIsText" | "BenchIsText" | "FileIsText" => "This edit needs the design's source documents",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::reason;

    #[test]
    fn refused_edit_reasons() {
        for (case, full, expected) in [
            ("reported paste", "drum16.ckt.splash:77:22: variable o not found in scope. Did you mean: e(obj502) or f(obj503), c(fn510), r(fn509), ts([7]) (+177 more)",
                "This edit uses the name o, which is unavailable here."),
            ("bench verification", "desk: probe out: splice verification failed: desk.bench.splash:4:1: variable out not found in scope",
                "This edit uses the name out, which is unavailable here."),
            ("net across blocks", "bias is not reachable inside mixer.out: give the block a port", "Give the block a port to reach net bias from mixer.out."),
            ("paste-local net", "shared net 2 is not in scope at this statement", "These parts cannot all reach the shared net here."),
            ("wire across blocks", "no scope names both pins: reg.c pin 1 and the new part (give the block a port)",
                "Give the block a port so you can connect reg.c pin 1 and the new part."),
            ("shared definition", "written in a body that 8 components share: give the block a port or a parameter",
                "This change would affect 8 parts, so give the block a port or a parameter to change one."),
            ("multiple pin sites", "reg.c pin A is connected in 2 places: mix.ckt.splash:2:3 (pins.A); mix.ckt.splash:4:8 (e.connect end 0): edit them by hand",
                "Edit the connections for reg.c pin A in the source because it is connected in 2 places."),
            ("nested part removal", "the component's call is inside another expression: remove it by hand",
                "Remove it in the source because the part is defined inside a larger expression."),
            ("computed value", "lamps[3].r: the value is computed by `e.series(o.r)`: edit the expression; inputs: `o.r` (parameter r)",
                "Change the expression in the source that sets the value of lamps[3].r."),
            ("rename collision", "\"out\" is already bound in this scope", "Choose another name because \"out\" is already in use here."),
            ("sheet layout", "the sheet table is written by the editor: write it as literal entries",
                "Write the sheet layout as individual entries so the editor can change them."),
            ("series insertion", "the pins to move are written in different blocks: move them one block at a time",
                "Move the pins one block at a time because they belong to different blocks."),
            ("board block", "Board(BlockItem { item: ItemId(2), block: BlockId(0) })", "This item moves only with its block."),
            ("board splice", "board op 1 (Move): NoItem(ItemId(2))", "This item is missing from the board."),
            ("computed bench control", "mcu.PWM: this value is computed here; edit the values it is made from",
                "Change the inputs that determine mcu.PWM in the bench source."),
            ("fallback", "mix.ckt.splash:4:3: object(obj502) and f(fn510) use 2.5 V (typical). More detail.",
                "Object and f use 2.5 V (typical)."),
            ("non-ASCII fallback", "élément indisponible! More detail.", "Élément indisponible!"),
            ("metadata alone", "mix.ckt.splash:2:7: (fn510)", "The edit could not be applied."),
            ("suggestion alone", ". Did you mean: e(obj502)", "The edit could not be applied."),
        ] {
            assert_eq!(reason(full), expected, "{case} should give the person the reason for the refusal");
        }
    }
}
