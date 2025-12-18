import { useRef, useEffect } from "react";

const Cursor = () => {
  const cursorRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const cursor = cursorRef.current;
    if (!cursor) return;
    let x = 0,
      y = 0;
    const move = (e: MouseEvent) => {
      (x = e.clientX), (y = e.clientY);
    };
    const loop = () => {
      cursor.style.transform = `translate3d(${x - 20}px, ${y - 20}px, 0)`;
      requestAnimationFrame(loop);
    };
    window.addEventListener("mousemove", move);
    requestAnimationFrame(loop);
    return () => {
      window.removeEventListener("mousemove", move);
    };
  }, []);
  return (
    <div
      className={`size-10 fixed z-10 bg-white border border-black pointer-events-none mix-blend-difference`}
      ref={cursorRef}
    />
  );
};

export default Cursor;
