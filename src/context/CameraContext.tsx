import React, { createContext, useContext, useState, useEffect, ReactNode } from 'react';
import { listen } from '@tauri-apps/api/event';

interface CameraParams {
  iso: string;
  shutterSpeed: string;
  aperture: string;
  exposureCompensation?: string;
  shootingMode?: string;
  whiteBalance?: string;
  focusMode?: string;
  driveMode?: string;
  meteringMode?: string;
  batteryLevel?: number | null;
  imagesRemaining?: number | null;
  model: string;
  port: string;
}

interface CameraContextType {
  isConnected: boolean;
  setIsConnected: (connected: boolean) => void;
  cameraParams: CameraParams | null;
  setCameraParams: (params: CameraParams | null) => void;
}

const CameraContext = createContext<CameraContextType | undefined>(undefined);

export const useCamera = () => {
  const context = useContext(CameraContext);
  if (!context) {
    throw new Error('useCamera must be used within CameraProvider');
  }
  return context;
};

export const CameraProvider: React.FC<{ children: ReactNode }> = ({ children }) => {
  const [isConnected, setIsConnected] = useState(false);
  const [cameraParams, setCameraParams] = useState<CameraParams | null>(null);

  // Listen for camera status events
  useEffect(() => {
    let unlisten: (() => void) | null = null;

    const setupListener = async () => {
      unlisten = (await listen('camera:status', (event: any) => {
        const status = event.payload;
        console.log('CameraContext: Status event:', status);

        if (status === 'Connected') {
          setIsConnected(true);
        } else if (status === 'Disconnected') {
          setIsConnected(false);
          setCameraParams(null);
        }
      })) as unknown as () => void;
    };

    setupListener();

    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  return (
    <CameraContext.Provider value={{ isConnected, setIsConnected, cameraParams, setCameraParams }}>
      {children}
    </CameraContext.Provider>
  );
};
