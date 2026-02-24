import { useState, useEffect } from "react";
import { VideoLibrary } from "./components/VideoLibrary";
import { DownloadForm } from "./components/DownloadForm";
import { ReviewScreen } from "./components/ReviewScreen";
import { Settings } from "./components/Settings";
import { ProcessingProvider } from "./lib/processing-context";
import { VideoInfo } from "./lib/types";
import { Download, Film, Settings as SettingsIcon, Clapperboard, ArrowUpCircle } from "lucide-react";
import { useUpdater } from "./hooks/useUpdater";

type View = "library" | "download" | "review" | "settings";

export default function App() {
  const [currentView, setCurrentView] = useState<View>("library");
  const [selectedVideo, setSelectedVideo] = useState<VideoInfo | null>(null);
  const updater = useUpdater();

  useEffect(() => {
    updater.checkForUpdates();
  }, []);

  const handleVideoSelect = (video: VideoInfo) => {
    // Only open review screen for videos that are ready for review
    if (video.status === "ready_for_review" || video.status === "ready") {
      setSelectedVideo(video);
      setCurrentView("review");
    }
    // For processing videos, just stay on library - they can watch progress
    // For downloaded videos, could optionally trigger processing here
  };

  const handleGoToLibrary = () => {
    setCurrentView("library");
  };

  const handleVideoUpdate = (updatedVideo: VideoInfo) => {
    setSelectedVideo(updatedVideo);
  };

  return (
    <div className="flex h-screen bg-background">
      {/* Sidebar */}
      <aside className="w-64 border-r border-border bg-card flex flex-col">
        <div className="p-4 border-b border-border">
          <h1 className="text-xl font-bold flex items-center gap-2">
            <Clapperboard className="w-6 h-6" />
            Sermon Cut
          </h1>
        </div>

        <nav className="flex-1 p-2">
          <NavButton
            icon={<Film className="w-5 h-5" />}
            label="Video Library"
            active={currentView === "library"}
            onClick={() => setCurrentView("library")}
          />
          <NavButton
            icon={<Download className="w-5 h-5" />}
            label="Download"
            active={currentView === "download"}
            onClick={() => setCurrentView("download")}
          />
          <NavButton
            icon={<SettingsIcon className="w-5 h-5" />}
            label="Settings"
            active={currentView === "settings"}
            onClick={() => setCurrentView("settings")}
          />
        </nav>

        {updater.available && (
          <div className="px-2 pb-2">
            <button
              onClick={() => setCurrentView("settings")}
              className="w-full px-3 py-2 bg-primary/10 text-primary rounded-lg hover:bg-primary/20 transition-colors text-sm flex items-center gap-2"
            >
              <ArrowUpCircle className="w-4 h-4" />
              Update v{updater.version} available
            </button>
          </div>
        )}
        <div className="p-4 border-t border-border text-sm text-muted-foreground">
          v0.1.0
        </div>
      </aside>

      {/* Main Content */}
      <main className="flex-1 overflow-auto relative">
        <ProcessingProvider>
          <div className={currentView === "library" ? "" : "hidden"}>
            <VideoLibrary onVideoSelect={handleVideoSelect} visible={currentView === "library"} />
          </div>
          <div className={currentView === "download" ? "" : "hidden"}>
            <DownloadForm onGoToLibrary={handleGoToLibrary} />
          </div>
          {selectedVideo && (
            <div className={currentView === "review" ? "" : "hidden"}>
              <ReviewScreen
                video={selectedVideo}
                onBack={() => setCurrentView("library")}
                onVideoUpdate={handleVideoUpdate}
              />
            </div>
          )}
          <div className={currentView === "settings" ? "" : "hidden"}>
            <Settings />
          </div>
        </ProcessingProvider>
      </main>
    </div>
  );
}

interface NavButtonProps {
  icon: React.ReactNode;
  label: string;
  active: boolean;
  onClick: () => void;
}

function NavButton({ icon, label, active, onClick }: NavButtonProps) {
  return (
    <button
      onClick={onClick}
      className={`w-full flex items-center gap-3 px-3 py-2 rounded-lg text-left transition-colors ${
        active
          ? "bg-primary text-primary-foreground"
          : "hover:bg-secondary text-foreground"
      }`}
    >
      {icon}
      {label}
    </button>
  );
}
