import { useRef, useState } from "react";
import * as THREE from "three";
import { useFrame } from "@react-three/fiber";
import {
  Environment,
  PerspectiveCamera,
  OrbitControls,
  ScrollControls,
  useScroll,
} from "@react-three/drei";

import gsap from "gsap";
import { useGSAP } from "@gsap/react";

import { FastrapiModel } from "../models/fastrapimodel";
import { CrystalField } from "../models/crystals/MultiChristal";

import { Rotate0, Rotate1, Rotate2, Rotate3 } from "./ContentRotate";
const rotateComponents = [Rotate0, Rotate1, Rotate2, Rotate3];
const length = rotateComponents.length;

const PI = Math.PI;

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
  const radius = 10;

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
      <Environment preset="dawn" />
      <ScrollControls pages={length} damping={0.25} infinite={false}>
        <PerspectiveCamera makeDefault position={[0, 0, 10]} ref={cameraRef} />
        <CameraScroller cameraRef={cameraRef} tl={tl} setCount={setCount} count={count} />
        <group ref={groupRef}>
          <CrystalField />
          <FastrapiModel ref={logoRef} />
          {(() => {
            const Rotate = rotateComponents[count];
            return Rotate ? <Rotate place={count % 2 === 1} /> : null;
          })()}
        </group>
      </ScrollControls>
    </>
  );
};

export default Experience;
