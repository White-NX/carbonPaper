import React, { useState, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import xibaoImg from '../assets/images/xibao.jpg';
import musicFile from '../assets/music/La Marcha Radetzky - Johann Strauss (1848).mp3';

/**
 * Critical error overlay displayed inside the main window.
 */
export default function ErrorWindow({ isVisible, errors = [], logPath = '', onRestart, onExit }) {
  const { t } = useTranslation();
  const [mode, setMode] = useState('xibao');
  const audioRef = useRef(null);

  // Control audio based on mode and visibility
  useEffect(() => {
    if (!audioRef.current) return;
    if (isVisible && mode === 'xibao') {
      audioRef.current.play().catch(() => {});
    } else {
      audioRef.current.pause();
      audioRef.current.currentTime = 0;
    }
  }, [mode, isVisible]);

  const toggleMode = () => {
    setMode((prev) => (prev === 'xibao' ? 'normal' : 'xibao'));
  };

  if (!isVisible) return null;

  const displayErrors = errors.length > 0 ? errors : [t('errorWindow.unknownError')];

  if (mode === 'xibao') {
    return (
      <div
        className="fixed inset-0 z-[100] overflow-auto flex flex-col"
        style={{
          backgroundImage: `url(${xibaoImg})`,
          backgroundSize: 'cover',
          backgroundPosition: 'center',
          backgroundRepeat: 'no-repeat',
        }}
      >
        <audio ref={audioRef} src={musicFile} autoPlay loop />

        <div
          className="flex-1 flex flex-col items-center px-20 pb-8"
          style={{ paddingTop: '120px' }}
        >
          <h1
            className="text-4xl font-black text-black mb-6 text-center"
            style={{ textShadow: '0 0 10px rgba(255,255,255,0.8)' }}
          >
            {t('errorWindow.title')}
          </h1>

          <div className="w-full max-w-lg space-y-3 mb-8">
            {displayErrors.map((err, i) => (
              <div
                key={i}
                className="error-rainbow-text text-2xl font-extrabold text-center break-words"
              >
                {err}
              </div>
            ))}
          </div>

          <div className="bg-white/80 backdrop-blur-sm rounded-xl p-5 max-w-lg w-full text-sm space-y-2">
            <p className="font-bold text-gray-800">{t('errorWindow.feedbackTitle')}</p>
            <p className="text-gray-700">
              {t('errorWindow.feedbackIssue')}{' '}
              <span className="text-blue-600 underline cursor-pointer select-all">
                github.com/White-NX/carbonPaper/issues
              </span>
            </p>
            {logPath && (
              <p className="text-gray-600 text-xs">
                {t('errorWindow.logLocation')}{' '}
                <code className="bg-gray-200 px-1.5 py-0.5 rounded text-gray-800 select-all break-all">
                  {logPath}
                </code>
              </p>
            )}
          </div>

          <div className="flex gap-4 mt-6">
            <button
              onClick={onRestart}
              className="px-6 py-2.5 bg-green-600 hover:bg-green-700 text-white rounded-lg font-bold text-sm transition-colors shadow-lg"
            >
              {t('errorWindow.restart')}
            </button>
            <button
              onClick={onExit}
              className="px-6 py-2.5 bg-red-600 hover:bg-red-700 text-white rounded-lg font-bold text-sm transition-colors shadow-lg"
            >
              {t('errorWindow.exit')}
            </button>
            <button
              onClick={toggleMode}
              className="px-4 py-2.5 bg-gray-600 hover:bg-gray-700 text-white rounded-lg text-xs transition-colors shadow-lg"
            >
              {t('errorWindow.switchToNormal')}
            </button>
          </div>
        </div>
      </div>
    );
  }

  // Normal mode
  return (
    <div className="fixed inset-0 z-[100] bg-ide-bg text-ide-text overflow-auto flex flex-col">
      <audio ref={audioRef} />

      <div className="flex-1 flex flex-col items-center justify-center px-10 py-8">
        <h1 className="text-2xl font-bold text-ide-error mb-4 text-center">
          {t('errorWindow.title')}
        </h1>

        <div className="w-full max-w-lg space-y-2 mb-6">
          {displayErrors.map((err, i) => (
            <div
              key={i}
              className="p-3 bg-red-500/10 border border-red-500/30 rounded-lg text-sm text-ide-text break-words"
            >
              {err}
            </div>
          ))}
        </div>

        <div className="bg-ide-panel border border-ide-border rounded-xl p-5 max-w-lg w-full text-sm space-y-2 mb-6">
          <p className="font-bold text-ide-text">{t('errorWindow.feedbackTitle')}</p>
          <p className="text-ide-muted">
            {t('errorWindow.feedbackIssue')}{' '}
            <span className="text-ide-accent underline cursor-pointer select-all">
              github.com/White-NX/carbonPaper/issues
            </span>
          </p>
          {logPath && (
            <p className="text-ide-muted text-xs">
              {t('errorWindow.logLocation')}{' '}
              <code className="bg-ide-bg px-1.5 py-0.5 rounded text-ide-text select-all break-all">
                {logPath}
              </code>
            </p>
          )}
        </div>

        <div className="flex gap-4">
          <button
            onClick={onRestart}
            className="px-6 py-2 bg-green-600 hover:bg-green-700 text-white rounded-lg font-medium text-sm transition-colors"
          >
            {t('errorWindow.restart')}
          </button>
          <button
            onClick={onExit}
            className="px-6 py-2 bg-red-600 hover:bg-red-700 text-white rounded-lg font-medium text-sm transition-colors"
          >
            {t('errorWindow.exit')}
          </button>
          <button
            onClick={toggleMode}
            className="px-4 py-2 bg-ide-panel hover:bg-ide-hover border border-ide-border text-ide-text rounded-lg text-xs transition-colors"
          >
            {t('errorWindow.switchToXibao')}
          </button>
        </div>
      </div>
    </div>
  );
}
