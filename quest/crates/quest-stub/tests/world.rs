//! The stub world's fixtures (plan Task 47): `people.txt`, `script.txt`, wire literals, and the people's
//! queries through the stub host.

use quest_stub::*;
use quest_vm::host::{Host, QueryError};
use quest_vm::wire::WireValue as W;

fn handle(name: &str, id: i64) -> W {
    W::Struct {
        name: name.into(),
        fields: vec![("id".into(), W::Int(id))],
    }
}

fn s(x: &str) -> W {
    W::Str(x.into())
}

const PEOPLE: &str = r#"
# the farm
person 3 name="Hal Brook" occupation=Farmer site=5 alive tags=pet,witness owner=7 pos=0,0,0
creature 7 name="Rufus" tags=pet owner=3 pos=2,0,1 kind=dog
player 1 name="Ana" skill.trade=3 inv.meat=1
site 5 kind=Village region=2
place 9 open_ground=0
fact act=1 loop=2
fact completed=errantry/x::Q
"#;

#[test]
fn people_and_script_parse() {
    let p = parse_people(PEOPLE).expect("parses");
    // A `#` inside a string, after an escaped quote, is no comment.
    let q = parse_people(r#"person 1 name="a \" # b" # a comment"#).expect("parses");
    assert_eq!(q.entities[&1].name, "a \" # b");
    let hal = &p.entities[&3];
    assert_eq!(hal.kind, EntityKind::Person);
    assert_eq!(hal.name, "Hal Brook");
    assert_eq!(hal.occupation.as_deref(), Some("Farmer"));
    assert_eq!((hal.site, hal.owner, hal.alive), (Some(5), Some(7), true));
    assert!(p.has_tag(3, "witness") && p.has_tag(7, "pet"));
    assert_eq!(p.pos[&7], [2, 0, 1]);
    assert_eq!(p.entities[&1].skills["trade"], 3);
    assert_eq!(p.entities[&1].inventory["meat"], 1);
    assert_eq!(p.entities[&1].kind, EntityKind::Player);
    assert_eq!(p.entities[&5].region, 2);
    assert_eq!(p.entities[&9].open_ground, Some(0));
    assert_eq!((p.facts["act"], p.facts["loop"]), (1, 2));
    assert!(p.completed.contains("errantry/x::Q"));
    assert_eq!(
        p.handle(1),
        Some(W::Struct {
            name: "Player".into(),
            fields: vec![("id".into(), W::Int(1)), ("name".into(), s("Ana"))],
        })
    );
    assert_eq!(p.handle(7), Some(handle("Creature", 7)));

    let script = parse_script(
        "1 talk 1 3   # Ana talks to Hal
2 pick 1 2
3 leave 1
3 done 1
4 event ItemGiven player=Player(1) to=Creature(7) item=Item(kind=\"meat\", count=1) @dog_watch
5 near 1 7
6 kill 7 by 1
6 kill 8
7 lose 7
8 ambient 3 4
9 edit next
",
    )
    .expect("parses");
    assert_eq!(script.len(), 11);
    assert_eq!(script[0], (1, Input::Talk { player: 1, npc: 3 }));
    assert_eq!(
        script[1],
        (
            2,
            Input::Pick {
                player: 1,
                option: 2
            }
        )
    );
    assert_eq!(
        script[4],
        (
            4,
            Input::Event {
                name: "ItemGiven".into(),
                fields: vec![
                    ("player".into(), "Player(1)".into()),
                    ("to".into(), "Creature(7)".into()),
                    ("item".into(), "Item(kind=\"meat\", count=1)".into()),
                ],
                part: Some("dog_watch".into()),
            }
        )
    );
    assert_eq!(
        script[6],
        (
            6,
            Input::Kill {
                victim: 7,
                by: Some(1)
            }
        )
    );
    assert_eq!(
        script[7],
        (
            6,
            Input::Kill {
                victim: 8,
                by: None
            }
        )
    );
    assert_eq!(script[9], (8, Input::Ambient { npcs: vec![3, 4] }));
    assert_eq!(script[10], (9, Input::Edit { dir: "next".into() }));

    // Wire literals: entity handles, a player's name from the people, enums, options, structs, lists.
    assert_eq!(
        parse_wire("Player(1)", &p),
        Ok(W::Struct {
            name: "Player".into(),
            fields: vec![("id".into(), W::Int(1)), ("name".into(), s("Ana"))],
        })
    );
    assert_eq!(parse_wire(" Person(3) ", &p), Ok(handle("Person", 3)));
    assert_eq!(
        parse_wire("Occupation::Smith", &p),
        Ok(W::Variant {
            name: "Occupation::Smith".into(),
            fields: vec![]
        })
    );
    assert_eq!(
        parse_wire("Role::dog", &p),
        Ok(W::Variant {
            name: "dog".into(),
            fields: vec![]
        })
    );
    assert_eq!(
        parse_wire("Some(-4)", &p),
        Ok(W::Variant {
            name: "Some".into(),
            fields: vec![("value".into(), W::Int(-4))]
        })
    );
    assert_eq!(
        parse_wire("[1, \"a b\", true, None]", &p),
        Ok(W::List(vec![
            W::Int(1),
            s("a b"),
            W::Bool(true),
            W::Variant {
                name: "None".into(),
                fields: vec![]
            }
        ]))
    );
}

#[test]
fn parse_errors_name_the_line() {
    let e = |text: &str| parse_people(text).expect_err("refused");
    assert!(
        e("person 3\nperson 3").starts_with("people line 2:"),
        "{}",
        e("person 3\nperson 3")
    );
    assert!(e("\n\nghost 1").starts_with("people line 3: unknown line kind"));
    assert!(e("person x").starts_with("people line 1:"));
    assert!(e("person 1 colour=red").contains("unknown key `colour`"));
    assert!(e("person 1 name=\"Hal").starts_with("people line 1:"));
    assert!(e("person 1 pos=1,2").contains("x,y,z"));
    assert!(e("fact era=3").contains("unknown fact"));
    assert!(
        e("\nperson 1 occupation=Wizard").starts_with("people line 2: `Wizard` is no occupation")
    );
    assert!(e("site 1 kind=Castle").starts_with("people line 1: `Castle` is no site kind"));

    let e = |text: &str| parse_script(text).expect_err("refused");
    assert!(e("1 talk 1").starts_with("script line 1:"));
    assert!(e("2 talk 1 3\n1 leave 1").starts_with("script line 2:"));
    assert!(e("1 dance 1").contains("unknown input `dance`"));
    assert!(e("1 pick 1 0").contains("from 1"));
    assert!(e("x talk 1 2").contains("not a tick"));
    assert!(e("1 leave 1\n\n1 event E f=Person(").starts_with("script line 3:"));
    assert!(e("1 event E f=Person(1").starts_with("script line 1:"));
    assert!(e("1 kill 3 with 1").starts_with("script line 1:"));

    assert!(parse_wire("Item(kind=1", &People::default()).is_err());
    assert!(parse_wire("1 2", &People::default()).is_err());
    let deep = format!("{}1{}", "[".repeat(100), "]".repeat(100));
    assert!(parse_wire(&deep, &People::default()).is_err());
}

#[test]
fn people_answer_the_quest_queries() {
    let mut h = StubHost::new(StubConfig::default());
    h.people = parse_people(PEOPLE).expect("parses");
    let (hal, rufus, ana) = (handle("Person", 3), handle("Creature", 7), {
        h.people.handle(1).expect("Ana")
    });
    let q = |h: &mut StubHost, f: u16, args: &[W]| h.query(f, args);
    assert_eq!(q(&mut h, F_ACT, &[]), Ok(W::Int(1)));
    assert_eq!(q(&mut h, F_LOOP, &[]), Ok(W::Int(2)));
    assert_eq!(
        q(&mut h, F_COMPLETED, &[s("errantry/x::Q")]),
        Ok(W::Bool(true))
    );
    assert_eq!(
        q(&mut h, F_PERSON_ALIVE, std::slice::from_ref(&hal)),
        Ok(W::Bool(true))
    );
    assert_eq!(
        q(&mut h, F_PERSON_OCCUPATION, std::slice::from_ref(&hal)),
        Ok(W::Variant {
            name: "Occupation::Farmer".into(),
            fields: vec![]
        })
    );
    assert_eq!(
        q(&mut h, F_CREATURE_OWNER, std::slice::from_ref(&rufus)),
        Ok(handle("Person", 3))
    );
    assert_eq!(
        q(&mut h, F_CREATURE_KIND, std::slice::from_ref(&rufus)),
        Ok(s("dog"))
    );
    assert_eq!(
        q(&mut h, F_PERSON_KIND, std::slice::from_ref(&hal)),
        Ok(s("person"))
    );
    assert_eq!(
        q(&mut h, F_CREATURE_NAME, std::slice::from_ref(&rufus)),
        Ok(s("Rufus"))
    );
    assert_eq!(
        q(&mut h, F_CREATURE_HAS_TAG, &[rufus.clone(), s("pet")]),
        Ok(W::Bool(true))
    );
    assert_eq!(
        q(&mut h, F_PERSON_SITE, std::slice::from_ref(&hal)),
        Ok(handle("Site", 5))
    );
    assert_eq!(
        q(&mut h, F_SITE_KIND, &[handle("Site", 5)]),
        Ok(W::Variant {
            name: "SiteKind::Village".into(),
            fields: vec![]
        })
    );
    assert_eq!(
        q(
            &mut h,
            F_PLACE_OPEN_GROUND,
            &[handle("Place", 9), W::Int(1)]
        ),
        Ok(W::Bool(false))
    );
    assert_eq!(
        q(&mut h, F_PLAYER_SKILL, &[ana.clone(), s("trade")]),
        Ok(W::Int(3))
    );
    assert_eq!(
        q(&mut h, F_PLAYER_INVENTORY_COUNT, &[ana.clone(), s("meat")]),
        Ok(W::Int(1))
    );
    // `near` by blocks (positions) and by regions (Hal and Rufus in region 0, the site in 2).
    let near = |a: &W, b: &W, r: i64, bl: i64| vec![a.clone(), b.clone(), W::Int(r), W::Int(bl)];
    assert_eq!(
        q(&mut h, F_NEAR, &near(&hal, &rufus, 0, 2)),
        Ok(W::Bool(true))
    );
    assert_eq!(
        q(&mut h, F_NEAR, &near(&hal, &rufus, 0, 1)),
        Ok(W::Bool(false))
    );
    assert_eq!(
        q(&mut h, F_NEAR, &near(&hal, &handle("Site", 5), 1, 0)),
        Ok(W::Bool(false))
    );
    assert_eq!(
        q(&mut h, F_NEAR, &near(&hal, &handle("Site", 5), 2, 0)),
        Ok(W::Bool(true))
    );
    assert_eq!(
        q(&mut h, F_RELATED, &[rufus.clone(), hal.clone()]),
        Ok(W::Bool(true))
    );
    assert_eq!(
        q(&mut h, F_PERSON_DISTANCE_TO, &[hal.clone(), rufus.clone()]),
        Ok(W::Int(2))
    );
    assert_eq!(
        q(&mut h, F_OURS, std::slice::from_ref(&rufus)),
        Ok(W::Bool(false))
    );
    // No site, no owner: refused, never a made-up `Site(0)` / `Person(0)`.
    assert!(matches!(
        q(&mut h, F_CREATURE_SITE, std::slice::from_ref(&rufus)),
        Err(QueryError::Failed(_))
    ));
    assert!(matches!(
        q(&mut h, F_PERSON_OWNER, &[handle("Person", 1)]),
        Err(QueryError::Failed(_))
    ));
    // An unknown entity is a fault, not a guess; a query the stub does not answer says which.
    assert!(matches!(
        q(&mut h, F_PERSON_ALIVE, &[handle("Person", 99)]),
        Err(QueryError::Failed(_))
    ));
    match q(&mut h, F_SAY, &[]) {
        Err(QueryError::Failed(m)) => assert!(m.contains("say"), "{m}"),
        other => panic!("{other:?}"),
    }
    // `give` and `take` change the inventory the queries read.
    let item = W::Struct {
        name: "Item".into(),
        fields: vec![("kind".into(), s("meat")), ("count".into(), W::Int(2))],
    };
    h.command(F_GIVE, &[ana.clone(), item]);
    h.command(F_TAKE, &[ana.clone(), s("meat"), W::Int(1)]);
    assert_eq!(
        q(&mut h, F_PLAYER_INVENTORY_COUNT, &[ana.clone(), s("meat")]),
        Ok(W::Int(2))
    );
    assert_eq!(h.output, ["give Ana 2 meat", "take Ana 1 meat"]);
}
