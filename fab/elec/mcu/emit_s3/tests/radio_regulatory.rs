use fab_elec_mcu_emit_s3::radio::regulatory::glue;
use fab_elec_mcu_island::GlueRegion;

#[test]
fn regulatory_glue_matches_the_s3_table_layout_and_limits() {
    let objects = glue();
    let [domains, profiles] = objects.as_slice() else { panic!("expected two regulatory tables") };
    assert_eq!(domains.name, "regdomain_table");
    assert_eq!(profiles.name, "regulatory_data");
    for object in &objects {
        assert_eq!(object.region, GlueRegion::Rodata, "{}", object.name);
        assert_eq!((object.align, object.entry), (4, 0), "{}", object.name);
        assert!(object.relocations.is_empty(), "{}", object.name);
    }

    const DOMAIN_ROWS: usize = 184;
    const PROFILE_COUNT: usize = 20;
    assert_eq!(domains.bytes.len(), DOMAIN_ROWS * 3);
    assert_eq!(profiles.bytes.len(), PROFILE_COUNT * 10);
    let mut countries = domains.bytes.chunks_exact(3);
    let sentinel = countries.next_back().unwrap();
    assert_eq!(&sentinel[..2], b"##");
    assert_eq!(usize::from(sentinel[2]), PROFILE_COUNT);
    for (index, code, profile) in [(0, b"01", 0), (1, b"EU", 10), (2, b"AD", 10), (DOMAIN_ROWS - 2, b"ZW", 10)] {
        let row = countries.clone().nth(index).unwrap();
        assert_eq!(&row[..2], code, "country row {index}");
        assert_eq!(row[2], profile, "profile at country row {index}");
    }
    for row in countries.clone() {
        assert_ne!(&row[..2], b"##", "only the final row is the sentinel");
        assert!(usize::from(row[2]) < PROFILE_COUNT, "invalid profile in country row {row:?}");
    }

    for (profile, row) in profiles.bytes.chunks_exact(10).enumerate() {
        let count = usize::from(row[0]);
        assert!((1..=2).contains(&count), "profile {profile} has {count} rules");
        assert_eq!(row[1], 0, "alignment byte in profile {profile}");
        let (active, unused) = row[2..].split_at(count * 4);
        for rule in active.chunks_exact(4) {
            let (first, last) = (rule[0], rule[1]);
            assert!(1 <= first && first <= last && last <= 14, "profile {profile} channels {first}..={last}");
            assert_eq!(rule[3] >> 2, 0, "reserved bits in profile {profile}");
        }
        assert!(unused.iter().all(|&byte| byte == 0), "unused rule in profile {profile}");
    }

    // AU's EIRP uses both bytes of the bitfields. JP fills both rule slots. TG sets DFS.
    let cases: &[(&[u8; 2], u8, &[[u8; 6]])] = &[
        (b"01", 0, &[[1, 11, 2, 20, 0, 0]]),
        (b"AU", 11, &[[1, 13, 2, 36, 0, 0]]),
        (b"JP", 8, &[[1, 13, 2, 20, 0, 0], [14, 14, 1, 20, 0, 0]]),
        (b"TG", 19, &[[1, 13, 2, 20, 1, 0]]),
    ];
    for &(country, profile, expected) in cases {
        let domain = countries.clone().find(|row| &row[..2] == country).expect("country present");
        assert_eq!(domain[2], profile, "profile for {country:?}");
        let row = profiles.bytes.chunks_exact(10).nth(usize::from(domain[2])).unwrap();
        assert_eq!(usize::from(row[0]), expected.len(), "rule count for {country:?}");
        for (rule, fields) in row[2..].chunks_exact(4).zip(expected) {
            let low = rule[2];
            let high = rule[3];
            let decoded = [rule[0], rule[1], low & 7, (low >> 3) | ((high & 1) << 5), (high >> 1) & 1, high >> 2];
            assert_eq!(&decoded, fields, "rule fields for {country:?}");
        }
    }
}
