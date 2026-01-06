import React, { useEffect, useState, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Invokes } from '../ui/AppProperties';
import { Camera, Plug2 } from 'lucide-react';
import { useCamera } from '../../context/CameraContext';
import Dropdown, { OptionItem } from '../ui/Dropdown';

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

interface CaptureResult {
  filePath: string;
  previewPath?: string | null;
  width: number;
  height: number;
}

interface CameraTetheringPanelProps {
  onImageSelect?: (path: string) => void;
  currentFolder?: string | null;
  refreshImageList?: () => void;
  createWorkspace?: () => Promise<string>;
}

// Config keys for different parameters
const CONFIG_KEYS = {
  iso: ['iso', 'isospeed', 'autoiso'],
  aperture: ['aperture', 'f-number', 'fnumber', 'aperture2'],
  shutter: ['shutterspeed', 'shutter', 'shutterspeed2', 'exptime', 'exposuretime'],
  exposure_comp: ['exposurecompensation', 'expcomp', 'exposurecomp', 'exposure'],
  white_balance: ['whitebalance', 'whitebalanceadjust', 'whitebalance2', 'wb'],
  focus_mode: ['focusmode', 'autofocus', 'afmode', 'focusmode2'],
  drive_mode: ['drivemode', 'capturemode', 'continuous'],
  shooting_mode: ['shootingmode', 'capturemode', 'capturemode2', 'autoexposuremode', 'exposuremode', 'mode'],
  metering_mode: ['meteringmode', 'meteringmodedial', 'metering'],
};

export const CameraTetheringPanel: React.FC<CameraTetheringPanelProps> = ({
  onImageSelect,
  currentFolder,
  refreshImageList,
  createWorkspace,
}) => {
  const { isConnected, setIsConnected, cameraParams, setCameraParams } = useCamera();
  const [isCapturing, setIsCapturing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Available choices for each parameter
  const [configChoices, setConfigChoices] = useState<Record<string, string[]>>({});
  const [isLoadingChoices, setIsLoadingChoices] = useState(false);

  // Track if we've set up listeners to avoid duplicates
  const listenersSetupRef = useRef(false);

  // Find the first working config key for a parameter
  const findWorkingConfigKey = useCallback(async (keys: string[]): Promise<string | null> => {
    for (const key of keys) {
      try {
        await invoke(Invokes.TetherGetConfigChoices, { configKey: key });
        return key;
      } catch {
        continue;
      }
    }
    return null;
  }, []);

  // Load available choices for all parameters when camera connects
  const loadConfigChoices = useCallback(async () => {
    if (!isConnected) return;

    setIsLoadingChoices(true);
    try {
      const choices: Record<string, string[]> = {};

      // Load choices for each parameter type
      const paramTypes = [
        { name: 'iso', keys: CONFIG_KEYS.iso },
        { name: 'aperture', keys: CONFIG_KEYS.aperture },
        { name: 'shutter', keys: CONFIG_KEYS.shutter },
        { name: 'exposure_comp', keys: CONFIG_KEYS.exposure_comp },
        { name: 'white_balance', keys: CONFIG_KEYS.white_balance },
        { name: 'focus_mode', keys: CONFIG_KEYS.focus_mode },
        { name: 'drive_mode', keys: CONFIG_KEYS.drive_mode },
        { name: 'shooting_mode', keys: CONFIG_KEYS.shooting_mode },
        { name: 'metering_mode', keys: CONFIG_KEYS.metering_mode },
      ];

      for (const param of paramTypes) {
        const workingKey = await findWorkingConfigKey(param.keys);
        if (workingKey) {
          try {
            const paramChoices: string[] = await invoke(Invokes.TetherGetConfigChoices, {
              configKey: workingKey,
            });
            choices[param.name] = paramChoices;
            // Store the working key for setting values later
            choices[`${param.name}_key`] = [workingKey];
          } catch {
            choices[param.name] = [];
          }
        }
      }

      setConfigChoices(choices);
    } catch (err) {
      console.error('Failed to load config choices:', err);
    } finally {
      setIsLoadingChoices(false);
    }
  }, [isConnected, findWorkingConfigKey]);

  // Get camera parameters
  const refreshParams = useCallback(async () => {
    if (!isConnected) return;
    try {
      const params: CameraParams = await invoke(Invokes.TetherGetParams);
      setCameraParams(params);
      setError(null);
    } catch (err) {
      // If we can't get params, the camera is disconnected
      if (isConnected) {
        setIsConnected(false);
        setCameraParams(null);
        setConfigChoices({});
      }
    }
  }, [isConnected, setIsConnected, setCameraParams]);

  // Set a config value
  const setConfigValue = useCallback(async (paramName: string, value: string) => {
    if (!isConnected) return;

    const workingKey = configChoices[`${paramName}_key`]?.[0];
    if (!workingKey) {
      console.error(`Cannot set ${paramName}: config key not found`);
      return;
    }

    try {
      await invoke(Invokes.TetherSetConfigValue, {
        configKey: workingKey,
        value,
      });
      // Refresh params to show updated value
      setTimeout(() => refreshParams(), 500);
    } catch (err) {
      console.error(`Failed to set ${paramName}:`, err);
    }
  }, [isConnected, configChoices, refreshParams]);

  // Update download folder to current workspace
  useEffect(() => {
    if (currentFolder) {
      invoke(Invokes.TetherSetDownloadFolder, { folder: currentFolder }).catch(err => {
        console.error('Failed to set camera download folder:', err);
      });
    }
  }, [currentFolder]);

  // Load config choices when camera connects
  useEffect(() => {
    if (isConnected && Object.keys(configChoices).length === 0) {
      loadConfigChoices();
    }
  }, [isConnected, configChoices, loadConfigChoices]);

  // Listen for camera status changes and capture events
  useEffect(() => {
    if (listenersSetupRef.current) return;

    const setupListeners = async () => {
      // Listen for camera status changes
      const unlistenStatus = await listen('camera:status', (event: any) => {
        const status = event.payload;
        if (status === 'Connected') {
          setIsConnected(true);
          setError(null);
          // Load config choices when connected
          setTimeout(() => {
            loadConfigChoices();
          }, 500);
          // Refresh params when connected
          setTimeout(() => {
            invoke(Invokes.TetherGetParams)
              .then((params: CameraParams) => setCameraParams(params))
              .catch(() => {});
          }, 300);
        } else if (status === 'Disconnected') {
          setIsConnected(false);
          setCameraParams(null);
          setConfigChoices({});
        }
      });

      // Listen for capture events
      const unlistenCaptured = await listen<CaptureResult>('camera:captured', (event) => {
        // File will be automatically opened by folder watcher
      });

      listenersSetupRef.current = true;

      return () => {
        unlistenStatus();
        unlistenCaptured();
      };
    };

    const cleanupPromise = setupListeners();
    return () => {
      cleanupPromise.then(cleanup => cleanup());
    };
  }, [setIsConnected, setCameraParams, loadConfigChoices]);

  // Capture photo
  const handleCapture = async () => {
    if (!isConnected) return;

    setIsCapturing(true);
    setError(null);
    try {
      // Determine target folder
      let targetFolder = currentFolder || undefined;

      // If no current workspace, create new one
      if (!targetFolder && createWorkspace) {
        targetFolder = await createWorkspace();
      }

      // Call capture command with target folder
      const result: CaptureResult = await invoke(Invokes.TetherCapture, {
        targetFolder,
      });

      // Refresh params to update remaining images count
      await refreshParams();
    } catch (err) {
      const errorMsg = err as string;
      setError(errorMsg);
    } finally {
      setIsCapturing(false);
    }
  };

  // Periodic refresh of camera parameters - real-time update (500ms)
  useEffect(() => {
    if (!isConnected) return;
    const interval = setInterval(refreshParams, 500);
    return () => clearInterval(interval);
  }, [isConnected, refreshParams]);

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="p-4 flex justify-between items-center flex-shrink-0 border-b border-surface">
        <h2 className="text-xl font-bold text-primary text-shadow-shiny">Tethering</h2>
      </div>

      {/* Content */}
      <div className="flex-grow overflow-y-auto p-4 text-text-secondary">
        {/* Camera Info */}
        {cameraParams && (
          <div className="mb-6">
            <h3 className="text-base font-bold text-text-primary mb-2 border-b border-surface pb-1 flex items-center gap-2">
              <Camera size={16} />
              Camera Info
            </h3>
            <div className="flex flex-col gap-1">
              <InfoItem label="Model" value={cameraParams.model} />
              <InfoItem label="Port" value={cameraParams.port} />
              {cameraParams.batteryLevel !== null && cameraParams.batteryLevel !== undefined && (
                <InfoItem label="Battery" value={`${Math.round(cameraParams.batteryLevel * 100)}%`} />
              )}
              {cameraParams.imagesRemaining !== null && cameraParams.imagesRemaining !== undefined && (
                <InfoItem label="Remaining" value={cameraParams.imagesRemaining.toString()} />
              )}
            </div>
          </div>
        )}

        {/* Exposure Controls */}
        {cameraParams && (
          <div className="mb-6">
            <h3 className="text-base font-bold text-text-primary mb-2 border-b border-surface pb-1">
              Exposure
            </h3>
            <div className="grid grid-cols-2 gap-2">
              <ConfigDropdown
                label="ISO"
                value={cameraParams.iso}
                choices={configChoices.iso || []}
                onChange={(v) => setConfigValue('iso', v)}
                disabled={isLoadingChoices}
              />
              <ConfigDropdown
                label="Aperture"
                value={cameraParams.aperture}
                choices={configChoices.aperture || []}
                onChange={(v) => setConfigValue('aperture', v)}
                disabled={isLoadingChoices}
              />
              <ConfigDropdown
                label="Shutter"
                value={cameraParams.shutterSpeed}
                choices={configChoices.shutter || []}
                onChange={(v) => setConfigValue('shutter', v)}
                disabled={isLoadingChoices}
              />
              <ConfigDropdown
                label="Exp. Comp"
                value={cameraParams.exposureCompensation || '--'}
                choices={configChoices.exposure_comp || []}
                onChange={(v) => setConfigValue('exposure_comp', v)}
                disabled={isLoadingChoices}
              />
            </div>
          </div>
        )}

        {/* Camera Settings */}
        {cameraParams && (
          <div className="mb-6">
            <h3 className="text-base font-bold text-text-primary mb-2 border-b border-surface pb-1">
              Camera Settings
            </h3>
            <div className="flex flex-col gap-2">
              <ConfigDropdown
                label="White Balance"
                value={cameraParams.whiteBalance || '--'}
                choices={configChoices.white_balance || []}
                onChange={(v) => setConfigValue('white_balance', v)}
                disabled={isLoadingChoices}
              />
              <ConfigDropdown
                label="Focus Mode"
                value={cameraParams.focusMode || '--'}
                choices={configChoices.focus_mode || []}
                onChange={(v) => setConfigValue('focus_mode', v)}
                disabled={isLoadingChoices}
              />
              <ConfigDropdown
                label="Drive Mode"
                value={cameraParams.driveMode || '--'}
                choices={configChoices.drive_mode || []}
                onChange={(v) => setConfigValue('drive_mode', v)}
                disabled={isLoadingChoices}
              />
              <ConfigDropdown
                label="Shooting Mode"
                value={cameraParams.shootingMode || '--'}
                choices={configChoices.shooting_mode || []}
                onChange={(v) => setConfigValue('shooting_mode', v)}
                disabled={isLoadingChoices}
              />
              <ConfigDropdown
                label="Metering Mode"
                value={cameraParams.meteringMode || '--'}
                choices={configChoices.metering_mode || []}
                onChange={(v) => setConfigValue('metering_mode', v)}
                disabled={isLoadingChoices}
              />
            </div>
          </div>
        )}

        {/* Capture Button */}
        {isConnected && (
          <div className="mb-4">
            <button
              onClick={handleCapture}
              disabled={isCapturing}
              className="w-full py-4 px-6 bg-surface hover:bg-bg-primary text-text-primary disabled:opacity-50 rounded-lg font-bold text-lg transition-colors flex items-center justify-center gap-2"
            >
              {isCapturing ? (
                <>
                  <div className="w-5 h-5 border-2 border-text-primary border-t-transparent rounded-full animate-spin" />
                  Capturing...
                </>
              ) : (
                <>
                  <Camera size={22} />
                  Capture Photo
                </>
              )}
            </button>
          </div>
        )}
      </div>
    </div>
  );
};

// Helper component for info display
function InfoItem({ label, value }: { label: string; value: string }) {
  return (
    <div className="grid grid-cols-3 gap-2 text-xs py-1.5 px-2 rounded odd:bg-bg-primary">
      <p className="font-semibold text-text-primary col-span-1 break-words">{label}</p>
      <p className="text-text-secondary col-span-2 break-words truncate" title={value}>
        {value}
      </p>
    </div>
  );
}

// Helper component wrapping the common Dropdown for camera config
function ConfigDropdown({
  label,
  value,
  choices,
  onChange,
  disabled,
}: {
  label: string;
  value: string;
  choices: string[];
  onChange: (value: string) => void;
  disabled?: boolean;
}) {
  // Convert string[] to OptionItem[] for the Dropdown component
  const options: OptionItem[] = choices.map(choice => ({
    label: choice,
    value: choice,
  }));

  // If no choices available, show a disabled display instead of dropdown
  if (choices.length === 0) {
    return (
      <div>
        <label className="text-xs text-text-secondary mb-1 block">{label}</label>
        <div className="w-full bg-surface border border-transparent rounded-md px-3 py-2 opacity-60">
          <span className="text-text-secondary">{value || 'Loading...'}</span>
        </div>
      </div>
    );
  }

  return (
    <div>
      <label className="text-xs text-text-secondary mb-1 block">{label}</label>
      <div className="[&_button]:!border-transparent [&_button]:!bg-surface">
        <Dropdown
          value={value}
          options={options}
          onChange={onChange}
          placeholder={label}
          className={disabled ? 'opacity-60' : ''}
        />
      </div>
    </div>
  );
}
