open Vitest
open TestingLibrary

afterEach(cleanup)

type surface = Inline | Conversation

let threadWithBody = (body: string): View.ThreadView.t => {
  let thread = Fixtures.parse(View.ThreadView.schema, "client", "ThreadView", "default")
  {
    ...thread,
    comments: [{...thread.comments->Array.getUnsafe(0), body}],
  }
}

let commentView = (surface, thread, dispatch) =>
  switch surface {
  | Inline => <InlineThread thread focused=true index=0 composer=React.null dispatch />
  | Conversation =>
    <Threads
      title="Conversation" threads=[thread] focus={Thread({index: 0})} indexOffset=0 dispatch
    />
  }

let select = (container, selector) => Element.querySelector(container, selector)->Nullable.getExn

describe("comment Markdown", () => {
  let snippet = "{\n\t\"engine\": \"example\",  \n\n  \"ready\": true\n}\n"
  let source =
    "`requestedWeight` is absent when it is zero. Follow-up: [controller issue](https://github.com/example/controller/issues/288).\n\n" ++
    "A second paragraph with **strong** and *emphasized* text.\n\n" ++
    "- First item\n- Second item\n  - Nested item\n\n" ++
    "3. Reproduce\n4. Verify\n\n```json\n" ++
    snippet ++ "```"

  test("inline and conversation bodies share formatting and preserve the source", () => {
    let thread = threadWithBody(source)
    let dispatch = fn()
    let {container, rerender} = render(commentView(Inline, thread, dispatch))
    let inlineBody = select(container, ".thread-body")->Element.innerHTML
    expect(select(container, "p code")->Element.textContent)->toBe("requestedWeight")
    expect(Element.querySelectorAll(container, ".thread-body p")->Array.length)->toBe(2)
    expect(Element.querySelectorAll(container, ".thread-body ul li")->Array.length)->toBe(3)
    expect(select(container, ".thread-body ul ul li")->Element.textContent)->toBe("Nested item")
    expect(select(container, ".thread-body ol")->Element.getAttribute("start"))->toEqual(
      Nullable.make("3"),
    )
    expect(select(container, "strong")->Element.textContent)->toBe("strong")
    expect(select(container, "em")->Element.textContent)->toBe("emphasized")
    expect(select(container, "pre code")->Element.textContent)->toBe(snippet)
    expect(select(container, "pre code")->Element.className)->toBe("language-json")
    expect(select(container, ".thread-body a")->Element.getAttribute("href"))->toEqual(
      Nullable.make("https://github.com/example/controller/issues/288"),
    )
    rerender(commentView(Conversation, thread, dispatch))
    expect(select(container, ".thread-body")->Element.innerHTML)->toBe(inlineBody)
    expect(thread.comments->Array.map(comment => comment.body))->toEqual([source])
    expect(dispatch)->not_->toHaveBeenCalled
  })

  [Inline, Conversation]->Array.forEach(surface => {
    let name = switch surface {
    | Inline => "inline"
    | Conversation => "conversation"
    }

    test(
      `${name}: plain text and line breaks remain readable`,
      () => {
        let body = "Plain text & a < b.\nA second line.\n\nAnother paragraph."
        let {container} = render(commentView(surface, threadWithBody(body), fn()))
        expect(select(container, ".thread-body p")->Element.textContent)->toBe(
          "Plain text & a < b.\nA second line.",
        )
        expect(Element.querySelectorAll(container, ".thread-body p")->Array.length)->toBe(2)
        expect(Screen.getByText("Another paragraph."))->toBeTruthy
      },
    )

    test(
      `${name}: raw HTML and script snippets are inert readable text`,
      () => {
        let raw = "<script>alert('example')</script>\n\n<img src=x onerror=alert('example')>\n\n<svg onload=alert('example')></svg>\n\nInline <b>HTML</b>."
        let {container} = render(commentView(surface, threadWithBody(raw), fn()))
        expect(
          Element.querySelector(container, "script, img, svg, b, [onerror], [onload]"),
        )->toBeNull
        expect(select(container, ".thread-body")->Element.textContent)->toContain(
          "<script>alert('example')</script>",
        )
        expect(select(container, ".thread-body")->Element.textContent)->toContain(
          "<img src=x onerror=alert('example')>",
        )
        expect(select(container, ".thread-body")->Element.textContent)->toContain(
          "<svg onload=alert('example')></svg>",
        )
        expect(select(container, ".thread-body")->Element.textContent)->toContain(
          "Inline <b>HTML</b>.",
        )
      },
    )

    test(
      `${name}: executable and local URL schemes never become links`,
      () => {
        let urls = [
          "javascript:alert(1)",
          "JaVaScRiPt:alert(1)",
          "java&#x73;cript:alert(1)",
          "javascript&#58;alert(1)",
          "java&#x09;script:alert(1)",
          "java&#x0A;script:alert(1)",
          "vbscript:msgbox(1)",
          "data:text/html;base64,PHNjcmlwdD4=",
          "file:///example/private.txt",
        ]
        let source =
          urls
          ->Array.mapWithIndex((url, i) => `[blocked ${Int.toString(i)}](<${url}>)`)
          ->Array.join("\n\n")
        let {container} = render(commentView(surface, threadWithBody(source), fn()))
        expect(Element.querySelector(container, ".thread-body a"))->toBeNull
        urls->Array.forEachWithIndex(
          (_, i) => {
            expect(Screen.getByText(`blocked ${Int.toString(i)}`))->toBeTruthy
          },
        )
      },
    )

    test(
      `${name}: safe links retain native mouse and keyboard activation`,
      () => {
        let dispatch = fn()
        let onKey = fn()
        let urls = [
          "https://github.com/example/project/issues/1",
          "http://example.com/issue",
          "mailto:ada@example.com",
          "../issues/1",
          "#note",
        ]
        let source =
          urls
          ->Array.mapWithIndex((url, i) => `[link ${Int.toString(i)}](${url} "Related issue")`)
          ->Array.join("\n\n")
        let {container} = render(
          <div onKeyDown=onKey> {commentView(surface, threadWithBody(source), dispatch)} </div>,
        )
        let links = Element.querySelectorAll(container, ".thread-body a")
        expect(links->Array.length)->toBe(urls->Array.length)
        links->Array.forEachWithIndex(
          (link, i) => {
            expect(Element.getAttribute(link, "href"))->toEqual(
              Nullable.make(urls->Array.getUnsafe(i)),
            )
            expect(Element.getAttribute(link, "target"))->toEqual(Nullable.make("_blank"))
            expect(Element.getAttribute(link, "rel"))->toEqual(Nullable.make("noopener noreferrer"))
            expect(Element.getAttribute(link, "title"))->toEqual(Nullable.make("Related issue"))
            Element.focus(link)
            expect(Document.activeElement)->toEqual(Nullable.make(link))
            FireEvent.keyDown(link, {"key": "Enter", "ctrlKey": false})
            FireEvent.click(link)
          },
        )
        expect(onKey)->not_->toHaveBeenCalled
        expect(dispatch)->not_->toHaveBeenCalled
      },
    )
  })
})
