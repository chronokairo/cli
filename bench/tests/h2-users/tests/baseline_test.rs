use users::service::UserService;

#[test]
fn service_starts_empty() {
    let svc = UserService::new();
    assert_eq!(svc.count(), 0);
}

#[test]
fn registered_user_is_retrievable_by_id() {
    let mut svc = UserService::new();
    let id = svc.register(String::from("gina"));
    assert_eq!(id, 0);
    assert_eq!(svc.count(), 1);
    assert_eq!(svc.get_user(id).map(|u| u.name.as_str()), Some("gina"));
}
