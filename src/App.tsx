import { useState, useEffect, useRef } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { emit, listen } from "@tauri-apps/api/event";
import { runBeforeLeave } from "./lib/beforeLeave";
import { VideoLibrary } from "./components/VideoLibrary";
import { DownloadForm } from "./components/DownloadForm";
import { ReviewScreen } from "./components/ReviewScreen";
import { Settings } from "./components/Settings";
import { ThumbnailGenerator } from "./components/ThumbnailGenerator";
import { getVideo, errorMessage } from "./lib/tauri";
import type { LibraryVideo, VideoDetail } from "./lib/types";
import { Download, Film, Settings as SettingsIcon, Clapperboard, ArrowUpCircle, Image } from "lucide-react";
import { UpdaterContext, useUpdaterState } from "./hooks/useUpdater";

type View = "library" | "download" | "review" | "settings" | "thumbnail";

export default function App() {
  const [currentView, setCurrentView] = useState<View>("library");
  const [selectedVideo, setSelectedVideo] = useState<VideoDetail | null>(null);
  // Bumped on every open so the review screen starts fresh from saved data.
  const [reviewKey, setReviewKey] = useState(0);
  const [appVersion, setAppVersion] = useState("");
  const updater = useUpdaterState();
  // Leaving was held up because the edits couldn't be saved.
  const [leaveBlocked, setLeaveBlocked] = useState<{ message: string; leave: () => void } | null>(null);
  const viewRef = useRef(currentView);
  viewRef.current = currentView;

  useEffect(() => {
    getVersion().then(setAppVersion).catch(() => {});
    updater.checkForUpdates();
  }, []);

  // Closing the window or quitting: save first. The app waits up to 2 s for
  // "flush-done", then goes ahead either way.
  useEffect(() => {
    const unlisten = listen("flush-before-exit", async () => {
      try {
        await runBeforeLeave();
      } catch (err) {
        console.error("Couldn't save before closing:", err);
      }
      await emit("flush-done");
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  /** Run `go` once whatever the open review has pending is saved. `always`:
   * check even when the review isn't the screen showing (it is about to be
   * replaced). If saving fails, stay and say why, with a way to leave anyway. */
  const leaveThen = async (go: () => void | Promise<void>, always = false) => {
    if (always || viewRef.current === "review") {
      try {
        await runBeforeLeave();
      } catch (err) {
        setLeaveBlocked({
          message: errorMessage(err, "Couldn't save your changes"),
          leave: () => {
            setLeaveBlocked(null);
            void go();
          },
        });
        return;
      }
    }
    setLeaveBlocked(null);
    await go();
  };

  const navigate = (view: View) => void leaveThen(() => setCurrentView(view));

  const handleVideoSelect = (video: LibraryVideo) => {
    // Only processed videos have something to review
    if (video.status !== "ready_for_review" && video.status !== "done" && video.status !== "uploading") return;
    // Opening reads the saved review, so the open one is saved first.
    void leaveThen(async () => {
      try {
        setSelectedVideo(await getVideo(video.id));
        setReviewKey((k) => k + 1);
        setCurrentView("review");
      } catch (err) {
        alert(errorMessage(err, "Couldn't open this video"));
      }
    }, true);
  };

  const handleGoToLibrary = () => navigate("library");

  return (
    <UpdaterContext.Provider value={updater}>
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
            onClick={() => navigate("library")}
          />
          <NavButton
            icon={<Download className="w-5 h-5" />}
            label="Download"
            active={currentView === "download"}
            onClick={() => navigate("download")}
          />
          <NavButton
            icon={<Image className="w-5 h-5" />}
            label="Thumbnail"
            active={currentView === "thumbnail"}
            onClick={() => navigate("thumbnail")}
          />
          <NavButton
            icon={<SettingsIcon className="w-5 h-5" />}
            label="Settings"
            active={currentView === "settings"}
            onClick={() => navigate("settings")}
          />
        </nav>

        {updater.available && (
          <div className="px-2 pb-2">
            <button
              onClick={() => navigate("settings")}
              className="w-full px-3 py-2 bg-primary/10 text-primary rounded-lg hover:bg-primary/20 transition-colors text-sm flex items-center gap-2"
            >
              <ArrowUpCircle className="w-4 h-4" />
              Update v{updater.version} available
            </button>
          </div>
        )}
        <div className="p-4 border-t border-border text-sm text-muted-foreground">
          {appVersion && `v${appVersion}`}
        </div>
      </aside>

      {/* Main Content */}
      <main className="flex-1 overflow-auto relative">
        {leaveBlocked && (
          <div role="alert" className="m-4 p-3 bg-destructive/10 text-destructive rounded-lg text-sm flex items-center justify-between gap-3">
            <span>{leaveBlocked.message}</span>
            <span className="flex gap-2 shrink-0">
              <button onClick={() => setLeaveBlocked(null)} className="px-3 py-1 bg-secondary text-secondary-foreground rounded-lg">
                Stay
              </button>
              <button onClick={leaveBlocked.leave} className="px-3 py-1 bg-destructive text-destructive-foreground rounded-lg">
                Leave anyway
              </button>
            </span>
          </div>
        )}
        <div className={currentView === "library" ? "" : "hidden"}>
          <VideoLibrary onVideoSelect={handleVideoSelect} visible={currentView === "library"} />
        </div>
        <div className={currentView === "download" ? "" : "hidden"}>
          <DownloadForm onGoToLibrary={handleGoToLibrary} isActive={currentView === "download"} />
        </div>
        {selectedVideo && (
          <div className={currentView === "review" ? "" : "hidden"}>
            <ReviewScreen
              key={`${selectedVideo.id}-${reviewKey}`}
              video={selectedVideo}
              onBack={handleGoToLibrary}
              active={currentView === "review"}
            />
          </div>
        )}
        <div className={currentView === "thumbnail" ? "" : "hidden"}>
          <ThumbnailGenerator />
        </div>
        <div className={currentView === "settings" ? "" : "hidden"}>
          <Settings />
        </div>
      </main>
    </div>
    </UpdaterContext.Provider>
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
