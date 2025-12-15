import { RotHeaderText, RotSubText } from "./Texts";
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
  return (
    <group>
      <RotHeaderText
        place={place}
        text="33x Faster"
        offset={[0, 0.5, 0]}
        size={isMobile ? 0.85 : 2}
      />
      <RotHeaderText place={place} text="than FastAPI" size={0.5} offset={[0, -0.5, 0]} />
      {/* feat: add many more here (performance metrics and other stuff) */}
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
