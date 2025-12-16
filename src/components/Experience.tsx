import { useRef, useState } from "react";
import * as THREE from "three";
import { useFrame } from "@react-three/fiber";
import {
  PerspectiveCamera,
  OrbitControls,
  ScrollControls,
  useScroll,
  SpotLight,
} from "@react-three/drei";

import gsap from "gsap";
import { useGSAP } from "@gsap/react";

import { FastrapiModel } from "../models/fastrapimodel";
import { CrystalField } from "../models/MultiCrystal";

import { Rotate0, Rotate1, Rotate2, Rotate3 } from "./ContentRotate";
import Buildings from "../models/Buildings";
import { CloudsMulti } from "../models/Cloud";

const rotateComponents = [Rotate0, Rotate1, Rotate2, Rotate3];
const length = rotateComponents.length;

const PI = Math.PI;
const radius = 10;

function CameraScroller({
  cameraRef,
  tl,
  setCount,
  count,
}: {
  cameraRef: React.RefObject<THREE.PerspectiveCamera>;
  tl: React.RefObject<gsap.core.Timeline>;
  setCount: React.Dispatch<React.SetStateAction<number>>;
  count: number;
}) {
  const scroll = useScroll();

  useFrame(() => {
    const angle = scroll.offset * (length - 1) * PI;
    const newCount = Math.floor((angle - PI / 2) / PI) + 1;
    if (newCount !== count) {
      setCount(newCount);
    }
    const x = Math.sin(angle) * radius;
    const z = Math.cos(angle) * radius;
    if (cameraRef.current) {
      cameraRef.current.position.set(x, 0, z);
      cameraRef.current.lookAt(0, 0, 0);
    }
    tl.current.seek(scroll.offset * tl.current.duration());
  });

  return null;
}

const Experience = () => {
  const logoRef = useRef<THREE.Mesh>(null!);
  const groupRef = useRef<THREE.Mesh>(null!);
  const tl = useRef<gsap.core.Timeline>(null!);
  const cameraRef = useRef<THREE.PerspectiveCamera>(null!);
  const [count, setCount] = useState<number>(1);

  useGSAP(() => {
    tl.current = gsap.timeline();
  }, []);

  return (
    <>
      <OrbitControls enableZoom={false} enablePan={false} />
      <SpotLight position={[0, 5, 0]} angle={1 / 2} intensity={50} rotation={[1, 1, 1]} />
      <ScrollControls pages={length} damping={0.25} infinite={false}>
        <PerspectiveCamera makeDefault position={[0, 0, radius]} ref={cameraRef} />
        <CameraScroller cameraRef={cameraRef} tl={tl} setCount={setCount} count={count} />
        <group ref={groupRef}>
          <CloudsMulti count={40} planeScale={100} offset={[5, 5, 0]} />
          <CrystalField />
          <Buildings
            min={[1, 1, 1]}
            max={[10, 10, 10]}
            avoidRadius={12}
            count={100}
            planeScale={75}
          />
          <FastrapiModel ref={logoRef} />
          {(() => {
            const Rotate = rotateComponents[count];
            return Rotate ? <Rotate place={count % 2 === 1} /> : null;
          })()}
          {/* {rotateComponents.map((RotateComponent, index) => (
            <group key={index} visible={count === index + 1}> 
                Note: Your original code used count (1-based) vs index (0-based).
                 Adjust logic: if count starts at 1, render visible if count === index + 1
               
              <RotateComponent place={(index + 1) % 2 === 1} />
            </group>
          ))} */}
        </group>
      </ScrollControls>
    </>
  );
};

export default Experience;
