# `scene-follow-up`: a follow-up `-> other_scene` is a scene's last step, and the target's cast may not be
# longer than the caller's (it is handed on by position).

scene duet(a: Person, b: Person):
    a: "One." @duet.one
    -> trio
    b: "Two." @duet.two

scene trio(a: Person, b: Person, c: Person):
    c: "Three." @trio.three

scene solo(a: Person):
    a: "Alone." @solo.alone
    -> trio
