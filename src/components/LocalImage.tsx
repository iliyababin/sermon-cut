import { useState, useEffect } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { ImageOff } from "lucide-react";

interface LocalImageProps {
  path: string;
  alt: string;
  className?: string;
}

export function LocalImage({ path, alt, className }: LocalImageProps) {
  const [error, setError] = useState(false);

  // Reset error state when path changes
  useEffect(() => {
    setError(false);
  }, [path]);

  if (error) {
    return (
      <div className={`flex items-center justify-center bg-muted ${className}`}>
        <ImageOff className="w-8 h-8 text-muted-foreground" />
      </div>
    );
  }

  const src = convertFileSrc(path);

  return (
    <img
      src={src}
      alt={alt}
      className={className}
      onError={() => setError(true)}
    />
  );
}
