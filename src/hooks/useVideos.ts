import { useState, useEffect, useCallback } from "react";
import { getVideos, deleteVideo as deleteVideoApi } from "@/lib/tauri";
import type { VideoInfo } from "@/lib/types";

export function useVideos() {
  const [videos, setVideos] = useState<VideoInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadVideos = useCallback(async () => {
    try {
      setLoading(true);
      const videoList = await getVideos();
      setVideos(videoList);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load videos");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadVideos();
  }, [loadVideos]);

  const deleteVideo = useCallback(async (videoId: string) => {
    try {
      await deleteVideoApi(videoId);
      setVideos((prev) => prev.filter((v) => v.id !== videoId));
    } catch (err) {
      throw err;
    }
  }, []);

  const addVideo = useCallback((video: VideoInfo) => {
    setVideos((prev) => [video, ...prev]);
  }, []);

  const updateVideo = useCallback((videoId: string, updates: Partial<VideoInfo>) => {
    setVideos((prev) =>
      prev.map((v) => (v.id === videoId ? { ...v, ...updates } : v))
    );
  }, []);

  return {
    videos,
    loading,
    error,
    loadVideos,
    deleteVideo,
    addVideo,
    updateVideo,
  };
}
