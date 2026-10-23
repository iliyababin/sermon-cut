export interface TemplateConfig {
  id: string;
  name: string;
  width: number;
  height: number;
  /**
   * Build the thumbnail's DOM inside `root` (which is emptied first) and
   * return the element to capture. Text goes in as text, never as markup.
   */
  render: (root: HTMLElement, data: TemplateData) => HTMLElement;
  /** Faces the template uses, as `document.fonts.load()` descriptors. */
  fonts: string[];
  /**
   * Adjust a rendered thumbnail to fit, e.g. shrink a long title. Called
   * with the element attached to the document and the fonts loaded, so
   * text can be measured.
   */
  fit?: (canvas: HTMLElement, data: TemplateData) => void;
}

export interface TemplateData {
  /** A URL the webview can load (already converted from a file path). */
  backgroundUrl: string;
  titleLines: string[];
  speakerName: string;
}
