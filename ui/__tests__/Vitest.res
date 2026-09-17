// Minimal vitest bindings for tests written in ReScript (the reference
// writes its tests in ReScript; the runner here is vitest).

@module("vitest") external describe: (string, unit => unit) => unit = "describe"
@module("vitest") external test: (string, unit => unit) => unit = "test"
/// A test whose body awaits (the clipboard's promise, say).
@module("vitest") external testAsync: (string, unit => promise<unit>) => unit = "test"
@module("vitest") external afterEach: (unit => unit) => unit = "afterEach"

type expect<'a>
@module("vitest") external expect: 'a => expect<'a> = "expect"
@send external toBe: (expect<'a>, 'a) => unit = "toBe"
@send external toEqual: (expect<'a>, 'a) => unit = "toEqual"
@send external toBeTruthy: expect<'a> => unit = "toBeTruthy"
@send external toContain: (expect<'a>, 'b) => unit = "toContain"
@send external toHaveBeenCalledTimes: (expect<'a>, int) => unit = "toHaveBeenCalledTimes"
@send external toHaveBeenCalledWith: (expect<'f>, 'a) => unit = "toHaveBeenCalledWith"
@send external toHaveBeenLastCalledWith: (expect<'f>, 'a) => unit = "toHaveBeenLastCalledWith"
@send external toMatchSnapshot: (expect<'a>, string) => unit = "toMatchSnapshot"
@get external not_: expect<'a> => expect<'a> = "not"
@send external toHaveBeenCalled: expect<'f> => unit = "toHaveBeenCalled"
@send external toBeNull: expect<'a> => unit = "toBeNull"

/// A mock function of one argument; `calls` are its arguments so far.
type mock<'a> = {mutable calls: array<array<'a>>}
@module("vitest") @scope("vi") external fn: unit => 'a => unit = "fn"
@get external mock: ('a => unit) => mock<'a> = "mock"
