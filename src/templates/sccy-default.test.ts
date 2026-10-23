import { describe, expect, it } from "vitest";
import { sccyDefault } from "./sccy-default";
import { cssUrl } from "./dom";

function render(data: Partial<Parameters<typeof sccyDefault.render>[1]> = {}) {
  const root = document.createElement("div");
  const canvas = sccyDefault.render(root, {
    backgroundUrl: "asset://localhost/bg.jpg",
    titleLines: ["The Hope", "We Have"],
    speakerName: "Pastor John",
    ...data,
  });
  return { root, canvas };
}

describe("sccy-default template", () => {
  it("puts the title and speaker in as text, never markup", () => {
    const { root } = render({
      titleLines: ["<3 & 'quotes'", "<img src=x onerror=alert(1)>"],
      speakerName: "<b>José</b>",
    });
    const title = root.querySelector("[data-role=title]")!;
    expect([...title.children].map((s) => s.textContent)).toEqual([
      "<3 & 'quotes'",
      "<img src=x onerror=alert(1)>",
    ]);
    expect(root.querySelector("img")).toBeNull();
    expect(root.querySelector("b")).toBeNull();
    expect(root.querySelector("[data-role=speaker]")!.textContent).toBe("<b>José</b>");
  });

  it("gives a path with an apostrophe or quote a valid url()", () => {
    const url = "asset://localhost/%2FUsers%2Fme%2FPastor's \"Best\" Sermon%2Fbg.jpg";
    const { root } = render({ backgroundUrl: url });
    const bg = root.querySelector<HTMLElement>("[data-role=background]")!;
    // jsdom drops a value it can't parse, so a non-empty one is valid CSS.
    expect(bg.style.backgroundImage).toBe(
      'url("asset://localhost/%2FUsers%2Fme%2FPastor%27s%20%22Best%22%20Sermon%2Fbg.jpg")',
    );
  });

  it("replaces what was in the root", () => {
    const root = document.createElement("div");
    root.append(document.createElement("p"));
    const canvas = sccyDefault.render(root, { backgroundUrl: "x", titleLines: ["A"], speakerName: "B" });
    expect(root.children).toHaveLength(1);
    expect(root.firstElementChild).toBe(canvas);
  });

  it("draws the logo as SVG letters", () => {
    const { root } = render();
    const letters = [...root.querySelectorAll("svg text")].map((t) => t.textContent);
    expect(letters).toEqual(["S", "C", "C", "Y"]);
    expect(root.querySelector("[data-role=logo]")).toBeNull();
  });

  it("draws the user's logo instead, when there is one", () => {
    const { root } = render({ logoUrl: "asset://localhost/%2FUsers%2Fme%2Flogo.png" });
    const logo = root.querySelector<HTMLImageElement>("img[data-role=logo]")!;
    expect(logo.getAttribute("src")).toBe("asset://localhost/%2FUsers%2Fme%2Flogo.png");
    expect(root.querySelector("svg")).toBeNull();
  });
});

describe("cssUrl", () => {
  it("percent-encodes what could break out of url()", () => {
    expect(cssUrl("asset://localhost/a%2Fb.jpg")).toBe('url("asset://localhost/a%2Fb.jpg")');
    expect(cssUrl("a'b(c) d")).toBe('url("a%27b%28c%29%20d")');
    expect(cssUrl('a"b\\c\nd')).toBe('url("a%22b%5Cc%0Ad")');
  });
});
