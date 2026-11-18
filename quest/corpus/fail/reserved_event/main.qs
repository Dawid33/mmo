# `reserved-name`: `Talked`, `DialogueEnded` and `SceneEnded` are the runner's own events; a script event of
# one of those names would hide it from every `@on` that names it.
event Talked:
    n: int

@on(Talked)
fn heard(e):
    print(f"{e.n}")
