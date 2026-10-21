export interface TemplateConfig {
  id: string;
  name: string;
  width: number;
  height: number;
  render: (data: TemplateData) => string;
}

export interface TemplateData {
  backgroundUrl: string;
  titleLines: string[];
  speakerName: string;
}
