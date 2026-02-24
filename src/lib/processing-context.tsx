import { createContext, useContext, useEffect, useState, ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import type { ProcessingProgress, ProcessingStage } from "./types";

interface ProcessingContextValue {
  activeJobs: Map<string, ProcessingProgress>;
  getJobProgress: (videoId: string) => ProcessingProgress | null;
}

const ProcessingContext = createContext<ProcessingContextValue | null>(null);

export function ProcessingProvider({ children }: { children: ReactNode }) {
  const [activeJobs, setActiveJobs] = useState<Map<string, ProcessingProgress>>(new Map());

  useEffect(() => {
    const unlisten = listen<{
      video_id: string;
      stage: ProcessingStage;
      progress: number;
      message: string;
    }>("processing-progress", (event) => {
      const { video_id, stage, progress, message } = event.payload;

      setActiveJobs((prev) => {
        const next = new Map(prev);
        next.set(video_id, {
          videoId: video_id,
          stage,
          progress,
          message,
        });
        return next;
      });

      // Remove completed jobs after a short delay
      if (stage === "complete") {
        setTimeout(() => {
          setActiveJobs((current) => {
            const updated = new Map(current);
            updated.delete(video_id);
            return updated;
          });
        }, 2000);
      }
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const getJobProgress = (videoId: string): ProcessingProgress | null => {
    return activeJobs.get(videoId) || null;
  };

  return (
    <ProcessingContext.Provider value={{ activeJobs, getJobProgress }}>
      {children}
    </ProcessingContext.Provider>
  );
}

export function useProcessing() {
  const context = useContext(ProcessingContext);
  if (!context) {
    throw new Error("useProcessing must be used within a ProcessingProvider");
  }
  return context;
}
