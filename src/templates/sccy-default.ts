import type { TemplateConfig, TemplateData } from "./types";

const SCCY_SVG_LOGO = `<svg viewBox="0 0 130 100" xmlns="http://www.w3.org/2000/svg">
  <rect x="20" y="5" width="90" height="90" fill="none" stroke="white" stroke-width="3"/>
  <line x1="3" y1="47" x2="127" y2="47" stroke="white" stroke-width="3"/>
  <line x1="3" y1="53" x2="127" y2="53" stroke="white" stroke-width="3"/>
  <text x="43" y="32" font-family="'Montserrat', sans-serif" font-size="39" font-weight="500" fill="white" text-anchor="middle" dominant-baseline="middle">S</text>
  <text x="87" y="32" font-family="'Montserrat', sans-serif" font-size="39" font-weight="500" fill="white" text-anchor="middle" dominant-baseline="middle">C</text>
  <text x="43" y="80" font-family="'Montserrat', sans-serif" font-size="39" font-weight="500" fill="white" text-anchor="middle" dominant-baseline="middle">C</text>
  <text x="87" y="80" font-family="'Montserrat', sans-serif" font-size="39" font-weight="500" fill="white" text-anchor="middle" dominant-baseline="middle">Y</text>
</svg>`;

/** Bump whenever the template's look changes, so stored renders are redone. */
export const TEMPLATE_VERSION = 1;

export const sccyDefault: TemplateConfig = {
  id: "sccy-default",
  name: "SCCY Sermon",
  width: 1280,
  height: 720,

  render(data: TemplateData): string {
    const titleHtml = data.titleLines
      .map((line) => `<span class="line">${line}</span>`)
      .join("");

    return `
<style>
  @font-face {
    font-family: 'Baskervville';
    font-style: normal;
    font-weight: 700;
    src: url('/fonts/Baskervville-Bold.woff2') format('woff2');
  }
  @font-face {
    font-family: 'Baskervville';
    font-style: normal;
    font-weight: 400;
    src: url('/fonts/Baskervville-Regular.woff2') format('woff2');
  }

  .thumbnail-canvas {
    position: relative;
    width: 1280px;
    height: 720px;
    background-color: #000;
    overflow: hidden;
  }

  .bg-image {
    position: absolute;
    top: 0;
    left: 0;
    width: 100%;
    height: 100%;
    background-size: cover;
    background-position: center right;
    z-index: 1;
  }

  .gradient-overlay {
    position: absolute;
    top: 0;
    left: 0;
    width: 50%;
    height: 100%;
    background: linear-gradient(to right,
      rgba(5, 8, 13, 1) 0%,
      rgba(5, 8, 13, 0.95) 35%,
      rgba(5, 8, 13, 0) 100%);
    z-index: 2;
  }

  .gradient-overlay-2 {
    position: absolute;
    top: 0;
    left: 0;
    width: 50%;
    height: 100%;
    background: linear-gradient(to right,
      rgba(5, 8, 13, 0.6) 0%,
      rgba(5, 8, 13, 0.4) 40%,
      rgba(5, 8, 13, 0) 100%);
    z-index: 2;
  }

  .logo-container {
    position: absolute;
    top: 80px;
    left: 60px;
    width: 99px;
    opacity: 0.50;
    z-index: 3;
  }

  .text-content {
    position: absolute;
    bottom: 100px;
    left: 80px;
    z-index: 3;
    display: flex;
    flex-direction: column;
    gap: 25px;
  }

  .main-title {
    font-family: 'Baskervville', serif;
    color: #ffffff;
    font-size: 100px;
    font-weight: 700;
    line-height: 1.0;
    margin: 0;
    text-shadow: 0px 4px 15px rgba(0,0,0,0.4);
  }

  .main-title .line {
    display: block;
  }

  .speaker-name {
    font-family: 'Baskervville', serif;
    color: #ffffff;
    font-size: 50px;
    font-weight: 400;
    margin: 0;
    opacity: 0.9;
    text-shadow: 0px 2px 10px rgba(0,0,0,0.4);
  }
</style>

<div class="thumbnail-canvas">
  <div class="bg-image" style="background-image: url('${data.backgroundUrl}');"></div>
  <div class="gradient-overlay"></div>
  <div class="gradient-overlay-2"></div>
  <div class="logo-container">${SCCY_SVG_LOGO}</div>
  <div class="text-content">
    <h1 class="main-title">${titleHtml}</h1>
    <h2 class="speaker-name">${data.speakerName}</h2>
  </div>
</div>`;
  },
};
