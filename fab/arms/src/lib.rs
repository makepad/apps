//! Fab Arms (experimental): a slicer for arm cells that works from the design and what the
//! cell has actually built. It plans deposits where material is missing and cuts where there
//! is excess, one task at a time from the as-built field as it stands; streams joint setpoints
//! planned through the arms' dynamic models; and corrects from every observation the cell
//! reports. It reaches the cell only through [`fab_robot::io::CellLink`]: the virtual cell, a
//! dry run or a real cell run the same code.

pub mod design;
pub mod fdm;
pub mod path;
pub mod planner;
pub mod route;
pub mod stats;
