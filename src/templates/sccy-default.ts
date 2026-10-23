import type { TemplateConfig, TemplateData } from "./types";
import { cssUrl, el, svg } from "./dom";
import { balanceLines } from "@/lib/titleSplit";

/** Bump whenever the template's look changes, so stored renders are redone. */
export const TEMPLATE_VERSION = 3;

/** The text may use the left of the frame up to this width... */
const MAX_TEXT_WIDTH = 1280 - 80 - 60;
/** ...and from below the logo (top 80 + 76) to 100px above the bottom. */
const MAX_TEXT_HEIGHT = 720 - 100 - 180;
const TITLE_SIZES = { largest: 100, smallest: 56, step: 4 };
const SPEAKER_SIZES = { largest: 50, smallest: 32, step: 2 };

function titleLine(text: string): HTMLElement {
  // max-content, so each line's width can be measured.
  return el("span", { display: "block", whiteSpace: "nowrap", width: "max-content" }, [text]);
}

/**
 * Shrink the title from 100px to 56px until it fits; if it doesn't, split it
 * into three lines and try again. The speaker line shrinks to fit its width.
 */
function fitText(canvas: HTMLElement, data: TemplateData): void {
  const title = canvas.querySelector<HTMLElement>("[data-role=title]");
  const speaker = canvas.querySelector<HTMLElement>("[data-role=speaker]");
  const block = title?.parentElement;
  if (!title || !speaker || !block) return;

  for (let size = SPEAKER_SIZES.largest; size >= SPEAKER_SIZES.smallest; size -= SPEAKER_SIZES.step) {
    speaker.style.fontSize = `${size}px`;
    if (speaker.offsetWidth <= MAX_TEXT_WIDTH) break;
  }

  const fits = () =>
    [...title.children].every((line) => (line as HTMLElement).offsetWidth <= MAX_TEXT_WIDTH) &&
    block.offsetHeight <= MAX_TEXT_HEIGHT;

  const text = data.titleLines.join(" ");
  const layouts = [data.titleLines];
  if (data.titleLines.length < 3) layouts.push(balanceLines(text, 3));
  for (const lines of layouts) {
    title.replaceChildren(...lines.map(titleLine));
    for (let size = TITLE_SIZES.largest; size >= TITLE_SIZES.smallest; size -= TITLE_SIZES.step) {
      title.style.fontSize = `${size}px`;
      if (fits()) return;
    }
  }
  // Still too big: the smallest size, wrapping within the width.
  title.style.maxWidth = `${MAX_TEXT_WIDTH}px`;
  for (const line of title.children) {
    Object.assign((line as HTMLElement).style, { whiteSpace: "normal", width: "auto" });
  }
}

const WHITE = "white";

/** The SCCY mark: a square crossed by two rules, with S C / C Y. */
function logo(): SVGElement {
  const letter = (x: number, y: number, ch: string) =>
    svg(
      "text",
      {
        x,
        y,
        "font-family": "'Montserrat', sans-serif",
        "font-size": 39,
        "font-weight": 500,
        fill: WHITE,
        "text-anchor": "middle",
        "dominant-baseline": "middle",
      },
      [ch],
    );
  return svg("svg", { viewBox: "0 0 130 100", width: 99, height: 76 }, [
    svg("rect", { x: 20, y: 5, width: 90, height: 90, fill: "none", stroke: WHITE, "stroke-width": 3 }),
    svg("line", { x1: 3, y1: 47, x2: 127, y2: 47, stroke: WHITE, "stroke-width": 3 }),
    svg("line", { x1: 3, y1: 53, x2: 127, y2: 53, stroke: WHITE, "stroke-width": 3 }),
    letter(43, 32, "S"),
    letter(87, 32, "C"),
    letter(43, 80, "C"),
    letter(87, 80, "Y"),
  ]);
}

const fill = { position: "absolute", top: "0", left: "0", height: "100%" } as const;

export const sccyDefault: TemplateConfig = {
  id: "sccy-default",
  name: "SCCY Sermon",
  width: 1280,
  height: 720,
  fonts: ["700 100px 'Baskervville'", "400 50px 'Baskervville'"],
  fit: fitText,

  render(root: HTMLElement, data: TemplateData): HTMLElement {
    const title = el(
      "h1",
      {
        fontFamily: "'Baskervville', serif",
        color: "#ffffff",
        fontSize: "100px",
        fontWeight: "700",
        lineHeight: "1.0",
        margin: "0",
        textShadow: "0px 4px 15px rgba(0,0,0,0.4)",
      },
      data.titleLines.map(titleLine),
    );
    title.dataset.role = "title";

    const speaker = el(
      "h2",
      {
        fontFamily: "'Baskervville', serif",
        color: "#ffffff",
        fontSize: "50px",
        fontWeight: "400",
        margin: "0",
        opacity: "0.9",
        whiteSpace: "nowrap",
        width: "max-content",
        textShadow: "0px 2px 10px rgba(0,0,0,0.4)",
      },
      [data.speakerName],
    );
    speaker.dataset.role = "speaker";

    const background = el("div", {
      ...fill,
      width: "100%",
      backgroundImage: cssUrl(data.backgroundUrl),
      backgroundSize: "cover",
      backgroundPosition: "center right",
      zIndex: "1",
    });
    background.dataset.role = "background";

    const canvas = el(
      "div",
      {
        position: "relative",
        width: "1280px",
        height: "720px",
        backgroundColor: "#000",
        overflow: "hidden",
      },
      [
        background,
        el("div", {
          ...fill,
          width: "50%",
          background:
            "linear-gradient(to right, rgba(5, 8, 13, 1) 0%, rgba(5, 8, 13, 0.95) 35%, rgba(5, 8, 13, 0) 100%)",
          zIndex: "2",
        }),
        el("div", {
          ...fill,
          width: "50%",
          background:
            "linear-gradient(to right, rgba(5, 8, 13, 0.6) 0%, rgba(5, 8, 13, 0.4) 40%, rgba(5, 8, 13, 0) 100%)",
          zIndex: "2",
        }),
        el("div", { position: "absolute", top: "80px", left: "60px", width: "99px", opacity: "0.50", zIndex: "3" }, [
          logo(),
        ]),
        el(
          "div",
          {
            position: "absolute",
            bottom: "100px",
            left: "80px",
            zIndex: "3",
            display: "flex",
            flexDirection: "column",
            gap: "25px",
          },
          [title, speaker],
        ),
      ],
    );
    root.replaceChildren(canvas);
    return canvas;
  },
};
