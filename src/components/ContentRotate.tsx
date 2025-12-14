import { RotHeaderText, RotSubText } from "./Texts";

export const Rotate0 = ({ place }: { place: boolean }) => {
  return (
    <group>
      <RotSubText place={place} text="Welcome to" size={0.5} offset={[0, 1, 0]} />
      <RotHeaderText place={place} text="FastRAPI" />
    </group>
  );
};

export const Rotate1 = ({ place }: { place: boolean }) => {
  return (
    <group>
      <RotHeaderText place={place} text="33x Faster" offset={[0, 0.5, 0]} />
      <RotHeaderText place={place} text="than FastAPI" size={0.5} offset={[0, -0.5, 0]} />
    </group>
  );
};

export const Rotate2 = ({ place }: { place: boolean }) => {
  return (
    <group>
      <RotSubText place={place} text="Type Safe support for" size={0.5} offset={[0, 1, 0]} />
      <RotHeaderText place={place} text="PyDantic" />
    </group>
  );
};
