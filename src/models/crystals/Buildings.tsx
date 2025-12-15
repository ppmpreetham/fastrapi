import { Instance, Instances } from "@react-three/drei";
import { useMemo } from "react";

type BuildingsProps = {
  min?: [number, number, number];
  max?: [number, number, number];
  avoidRadius?: number;
  count?: number;
  offset?: number;
};

const Buildings = ({
  min = [0.5, 1, 0.5],
  max = [2, 10, 2],
  avoidRadius = 12,
  count = 50,
  offset = -2,
}: BuildingsProps) => {
  const planeScale = 75;

  const buildings = useMemo(() => {
    const result = [];
    let attempts = 0;
    const maxAttempts = count * 100;

    while (result.length < count && attempts < maxAttempts) {
      const x = (Math.random() - 0.5) * planeScale;
      const z = (Math.random() - 0.5) * planeScale;

      attempts++;

      if (Math.sqrt(x * x + z * z) < avoidRadius) {
        continue;
      }

      const scaleX = Math.random() * (max[0] - min[0]) + min[0];
      const scaleY = Math.random() * (max[1] - min[1]) + min[1];
      const scaleZ = Math.random() * (max[2] - min[2]) + min[2];

      result.push({
        position: [x, scaleY / 2 + offset, z] as [number, number, number],
        scale: [scaleX, scaleY, scaleZ] as [number, number, number],
        key: result.length,
      });
    }

    console.log(`Generated ${result.length} buildings in ${attempts} attempts`);
    return result;
  }, []);

  return (
    <group>
      <Instances>
        <boxGeometry args={[1, 1, 1]} />
        <meshStandardMaterial color="white" />
        {buildings.map((building) => (
          <Instance key={building.key} position={building.position} scale={building.scale} />
        ))}
      </Instances>
    </group>
  );
};

export default Buildings;
