import { useState } from "react";
import type { TranscriptionResult, Chapter } from "@/lib/types";
import { Scissors, Clock, Loader2 } from "lucide-react";

interface TranscriptReviewProps {
  transcription: TranscriptionResult;
  videoDuration: number;
  onTrim: (startTime: number, endTime: number) => void;
  processing: boolean;
}

export function TranscriptReview({
  transcription,
  videoDuration,
  onTrim,
  processing,
}: TranscriptReviewProps) {
  const [startTime, setStartTime] = useState<number>(
    transcription.sermon_start ?? 0
  );
  const [endTime, setEndTime] = useState<number>(
    transcription.sermon_end ?? videoDuration
  );
  const [title, setTitle] = useState(transcription.suggested_title ?? "");
  const [description, setDescription] = useState(
    transcription.suggested_description ?? ""
  );
  const [chapters, setChapters] = useState<Chapter[]>(
    transcription.suggested_chapters ?? []
  );

  function handleTrim() {
    onTrim(startTime, endTime);
  }

  return (
    <div className="max-w-4xl mx-auto space-y-6">
      {/* Sermon Boundaries */}
      <div className="bg-card border border-border rounded-lg p-6">
        <h3 className="text-lg font-semibold mb-4">Sermon Boundaries</h3>

        <div className="grid grid-cols-2 gap-4">
          <div>
            <label className="block text-sm font-medium mb-2">Start Time</label>
            <div className="flex items-center gap-2">
              <input
                type="text"
                value={formatTime(startTime)}
                onChange={(e) => setStartTime(parseTime(e.target.value))}
                className="flex-1 px-4 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring font-mono"
              />
              <span className="text-sm text-muted-foreground">
                ({startTime.toFixed(1)}s)
              </span>
            </div>
          </div>

          <div>
            <label className="block text-sm font-medium mb-2">End Time</label>
            <div className="flex items-center gap-2">
              <input
                type="text"
                value={formatTime(endTime)}
                onChange={(e) => setEndTime(parseTime(e.target.value))}
                className="flex-1 px-4 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring font-mono"
              />
              <span className="text-sm text-muted-foreground">
                ({endTime.toFixed(1)}s)
              </span>
            </div>
          </div>
        </div>

        <p className="mt-4 text-sm text-muted-foreground">
          Duration: {formatTime(endTime - startTime)}
        </p>
      </div>

      {/* Metadata */}
      <div className="bg-card border border-border rounded-lg p-6">
        <h3 className="text-lg font-semibold mb-4">Video Metadata</h3>

        <div className="space-y-4">
          <div>
            <label className="block text-sm font-medium mb-2">Title</label>
            <input
              type="text"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              className="w-full px-4 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring"
            />
          </div>

          <div>
            <label className="block text-sm font-medium mb-2">Description</label>
            <textarea
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              rows={4}
              className="w-full px-4 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring resize-none"
            />
          </div>
        </div>
      </div>

      {/* Chapters */}
      {chapters.length > 0 && (
        <div className="bg-card border border-border rounded-lg p-6">
          <h3 className="text-lg font-semibold mb-4">Chapters</h3>

          <div className="space-y-2">
            {chapters.map((chapter, index) => (
              <div
                key={index}
                className="flex items-center gap-4 p-2 bg-muted rounded-lg"
              >
                <span className="font-mono text-sm text-muted-foreground">
                  {formatTime(chapter.time)}
                </span>
                <input
                  type="text"
                  value={chapter.title}
                  onChange={(e) => {
                    const newChapters = [...chapters];
                    newChapters[index] = { ...chapter, title: e.target.value };
                    setChapters(newChapters);
                  }}
                  className="flex-1 px-3 py-1 border border-input rounded focus:outline-none focus:ring-2 focus:ring-ring text-sm"
                />
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Transcript */}
      <div className="bg-card border border-border rounded-lg p-6">
        <h3 className="text-lg font-semibold mb-4">Transcript</h3>

        <div className="max-h-96 overflow-auto space-y-2">
          {transcription.segments.map((segment, index) => {
            const isInRange =
              segment.start >= startTime && segment.end <= endTime;

            return (
              <div
                key={index}
                className={`flex gap-4 p-2 rounded-lg ${
                  isInRange ? "bg-green-50" : "bg-muted/50"
                }`}
              >
                <span className="flex-shrink-0 text-xs font-mono text-muted-foreground flex items-center gap-1">
                  <Clock className="w-3 h-3" />
                  {formatTime(segment.start)}
                </span>
                <p className="text-sm">{segment.text}</p>
              </div>
            );
          })}
        </div>
      </div>

      {/* Actions */}
      <div className="flex justify-end gap-4">
        <button
          onClick={handleTrim}
          disabled={processing}
          className="px-6 py-3 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 disabled:opacity-50 flex items-center gap-2 transition-colors"
        >
          {processing ? (
            <>
              <Loader2 className="w-5 h-5 animate-spin" />
              Trimming...
            </>
          ) : (
            <>
              <Scissors className="w-5 h-5" />
              Trim Video
            </>
          )}
        </button>
      </div>
    </div>
  );
}

function formatTime(seconds: number): string {
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const secs = Math.floor(seconds % 60);

  if (hours > 0) {
    return `${hours}:${minutes.toString().padStart(2, "0")}:${secs.toString().padStart(2, "0")}`;
  }
  return `${minutes}:${secs.toString().padStart(2, "0")}`;
}

function parseTime(timeStr: string): number {
  const parts = timeStr.split(":").map(Number);

  if (parts.length === 3) {
    return parts[0] * 3600 + parts[1] * 60 + parts[2];
  } else if (parts.length === 2) {
    return parts[0] * 60 + parts[1];
  }

  return Number(timeStr) || 0;
}
