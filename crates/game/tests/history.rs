use game::history::*;

#[test]
fn calendar_dates() {
    assert_eq!(CalendarTick(0).date(), Date { year: 0, month: 1, day: 1 });
    let one_year = CalendarTick::from_days(DAYS_PER_YEAR);
    assert_eq!(one_year.date(), Date { year: 1, month: 1, day: 1 });
    assert_eq!(CalendarTick::from_days(DAYS_PER_MONTH + 2).date(), Date { year: 0, month: 2, day: 3 });
    assert_eq!(one_year.years_since(CalendarTick(0)), 1);
    assert_eq!(TICKS_PER_DAY, 200);
    assert_eq!(CalendarTick::from_days(2).date().to_string(), "3 Granite 0");
}

#[test]
fn person_age_and_alive() {
    let p = Person::newborn(PersonId(1), Sex::Female, CalendarTick(0), None, None, None, Location::AtSite(SiteId(0)));
    assert!(p.alive());
    assert_eq!(p.age_years(CalendarTick::from_days(DAYS_PER_YEAR * 30 + 5)), 30);
}
