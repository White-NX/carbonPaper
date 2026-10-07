import React from 'react';
import appMark from '../assets/carbonpaper-mark.svg';

export default function BrandMark({ className = '' }) {
  return (
    <span
      aria-hidden="true"
      className={`block shrink-0 bg-current ${className}`}
      style={{
        mask: `url("${appMark}") center / contain no-repeat`,
        WebkitMask: `url("${appMark}") center / contain no-repeat`,
      }}
    />
  );
}
