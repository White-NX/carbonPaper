import React, { useState, useRef, useEffect, useCallback } from 'react';
import PropTypes from 'prop-types';
import { GripVertical } from 'lucide-react';
import { Dialog } from './Dialog';

export function ImageCompareDialog({ isOpen, onClose, image1, image2 }) {
  const [position, setPosition] = useState(50);
  const containerRef = useRef(null);
  const isDragging = useRef(false);

  const handleMove = useCallback((clientX) => {
    if (!containerRef.current) return;
    const rect = containerRef.current.getBoundingClientRect();
    const x = clientX - rect.left;
    const newPos = Math.max(0, Math.min(100, (x / rect.width) * 100));
    setPosition(newPos);
  }, []);

  const handleMouseDown = () => {
    isDragging.current = true;
  };

  const handleMouseUp = useCallback(() => {
    isDragging.current = false;
  }, []);

  const handleMouseMove = useCallback((e) => {
    if (isDragging.current) {
      handleMove(e.clientX);
    }
  }, [handleMove]);

  const handleTouchMove = useCallback((e) => {
    if (isDragging.current) {
      handleMove(e.touches[0].clientX);
    }
  }, [handleMove]);

  useEffect(() => {
    if (isOpen) {
      window.addEventListener('mouseup', handleMouseUp);
      window.addEventListener('mousemove', handleMouseMove);
      window.addEventListener('touchend', handleMouseUp);
      window.addEventListener('touchmove', handleTouchMove);
    }
    return () => {
      window.removeEventListener('mouseup', handleMouseUp);
      window.removeEventListener('mousemove', handleMouseMove);
      window.removeEventListener('touchend', handleMouseUp);
      window.removeEventListener('touchmove', handleTouchMove);
    };
  }, [isOpen, handleMouseUp, handleMouseMove, handleTouchMove]);

  return (
    <Dialog
      isOpen={isOpen}
      onClose={onClose}
      title="Compare Images"
      maxWidth="max-w-5xl"
      contentClassName="flex-1 relative overflow-hidden bg-black/50 flex items-center justify-center p-4 min-h-[400px]"
    >
        <div 
            ref={containerRef}
            className="relative select-none cursor-col-resize max-h-full max-w-full aspect-auto"
            onMouseDown={handleMouseDown}
            onTouchStart={handleMouseDown}
        >
            {/* Base Image (Right side / After) */}
            <img 
                src={image2} 
                alt="Second" 
                className="max-h-[calc(90vh-120px)] object-contain pointer-events-none select-none" 
                draggable={false}
            />

            {/* Overlay Image (Left side / Before) - Clipped */}
            <div 
                className="absolute inset-0 overflow-hidden pointer-events-none select-none"
                style={{ width: `${position}%` }}
            >
                <img 
                    src={image1} 
                    alt="First" 
                    className="h-full w-full max-w-none object-contain object-left" 
                    draggable={false}
                    style={{ 
                        width: containerRef.current ? `${containerRef.current.clientWidth}px` : '100%',
                        height: '100%'
                    }}
                />
            </div>

            {/* Slider Handle */}
            <div 
                className="absolute top-0 bottom-0 w-1 bg-white cursor-col-resize shadow-[0_0_10px_rgba(0,0,0,0.5)] flex items-center justify-center hover:bg-ide-accent transition-colors"
                style={{ left: `${position}%`, transform: 'translateX(-50%)' }}
            >
                <div className="w-6 h-6 bg-white rounded-full shadow-md flex items-center justify-center text-black">
                    <GripVertical className="w-4 h-4" />
                </div>
            </div>
            
            {/* Labels */}
            <div className="absolute top-4 left-4 bg-black/60 text-white text-xs px-2 py-1 rounded pointer-events-none">
                Image 1
            </div>
            <div className="absolute top-4 right-4 bg-black/60 text-white text-xs px-2 py-1 rounded pointer-events-none">
                Image 2
            </div>
        </div>
    </Dialog>
  );
}

ImageCompareDialog.propTypes = {
  isOpen: PropTypes.bool.isRequired,
  onClose: PropTypes.func.isRequired,
  image1: PropTypes.string,
  image2: PropTypes.string,
};
