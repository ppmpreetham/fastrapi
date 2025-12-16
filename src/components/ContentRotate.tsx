import { RotHeaderText, RotSubText } from "./Texts";
import { Html } from "@react-three/drei";
import FrameworkChart from "./Chart";
import { useThree } from "@react-three/fiber";

var isMobile =
  typeof window !== "undefined" && /Mobi|Android|iPhone|iPad|iPod/i.test(navigator.userAgent);

export const Rotate0 = ({ place }: { place: boolean }) => {
  return (
    <group>
      <RotSubText place={place} text="Welcome to" size={0.5} offset={[0, 1, 0]} />
      <RotHeaderText place={place} text="FastRAPI" size={isMobile ? 1 : 2} />
    </group>
  );
};

export const Rotate1 = ({ place }: { place: boolean }) => {
  const { gl } = useThree(); // Get the canvas parent
  const portalContainer =
    gl.domElement.parentNode instanceof HTMLElement ? gl.domElement.parentNode : undefined; // Ensure it's an HTMLElement
  return (
    <group>
      <Html
        position={[3, 3.5, -1]}
        center
        scale={isMobile ? 0.7 : 0.3}
        transform
        rotation={[0, Math.PI, 0]}
        // occlude={false}
        zIndexRange={[0, 0]}
        portal={{ current: gl.domElement.parentNode as HTMLElement }}
        style={{
          position: "fixed",
          transform: "translate(-50%, -50%)",
          pointerEvents: "none",
        }}
      >
        <div className="pointer-events-auto max-w-full">
          <FrameworkChart />
        </div>
      </Html>
      <RotHeaderText
        place={place}
        text="33x Faster"
        offset={[0, 0, 0]}
        size={isMobile ? 0.85 : 2}
      />
      <RotHeaderText place={place} text="than FastAPI" size={0.5} offset={[0, -1, 0]} />
    </group>
  );
};

export const Rotate2 = ({ place }: { place: boolean }) => {
  return (
    <group>
      <RotSubText place={place} text="Features Include" size={0.5} offset={[0, 1, 0]} />
      <RotHeaderText place={place} text="PyDantic" size={isMobile ? 1.1 : 2} />
      {/* feat: add many more here (images and other stuff) */}
    </group>
  );
};
export const Rotate3 = ({ place }: { place: boolean }) => {
  return (
    <group>
      <RotSubText place={place} text="Just change" size={0.5} offset={[0, 1, 0]} />
      <RotHeaderText place={place} text="ONE CHARACTER" />
      {/* feat: show change from FastAPI to FastRAPI */}
    </group>
  );
};
