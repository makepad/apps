pub const BLINK: &str = "\
let led = output(PB5)
let button = input(PD2, pull_up)
var on = false
var ticks: i32 = 0
var stamp_ms: i32 = 0
var pressed: i32 = 0

fn setup() {
    write(led, low)
}

let blink = every(ms(250), fn() {
    on = !on
    ticks = ticks + 1
    write(led, if on { high } else { low })
    stamp_ms = now_ms()
    pressed = read(button)
    if ticks == 6 {
        stop(blink)
    }
})

let again = after(ms(2000), fn() {
    start(blink)
})
";
