use crate::actions::register_actions;
use fab_elec_design::action::{catalog, find, FieldKind, Risk};

#[test]
fn register_find_and_repeat_manufacturing_actions() {
    let expected = [
        ("elec.export", Risk::External),
        ("elec.export.validate", Risk::Read),
        ("elec.factory.set", Risk::Edit),
    ];
    let expected_names: Vec<_> = expected.iter().map(|(name, _)| *name).collect();

    // Re-registering replaces entries by name, so both passes must have the same entries.
    for _ in 0..2 {
        register_actions();

        for (name, risk) in expected {
            let action = find(name).unwrap_or_else(|| panic!("missing action {name}"));
            assert_eq!(action.group, "Fabricate", "group for {name}");
            assert_eq!(action.risk, risk, "risk for {name}");
        }

        let factory = find("elec.factory.set").unwrap();
        let profile = factory
            .fields
            .iter()
            .find(|field| field.name == "profile")
            .expect("factory.set has a profile field");
        assert!(profile.required, "factory.set requires a profile");
        assert_eq!(profile.kind, FieldKind::Text, "profile is a name");

        let entries = catalog();
        assert!(
            entries.windows(2).all(|pair| pair[0].name < pair[1].name),
            "catalog names are unique and sorted"
        );
        let names: Vec<_> = entries
            .iter()
            .map(|action| action.name)
            .filter(|name| expected_names.contains(name))
            .collect();
        assert_eq!(
            names, expected_names,
            "each manufacturing action appears once in catalog order after every registration"
        );
    }
}
