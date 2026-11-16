enum Stage:
    Asked
    Accepted(tick: int)
    Done

fn talk(s: Stage):
    match s:
        case Stage::Asked:
            print("asked")
        case Stage::Done:
            print("done")

fn main():
    talk(Stage::Done)
