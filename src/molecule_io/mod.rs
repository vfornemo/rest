use rayon::prelude::{IntoParallelRefIterator, IndexedParallelIterator, ParallelIterator};
use rest_tensors::{ERIFull,RIFull,ERIFold4,TensorSlice,TensorSliceMut,TensorOptMut,TensorOpt, MatrixUpper, MatrixFull};
use libc::regerror;
use tensors::BasicMatrix;
use tensors::external_libs::{ri_copy_from_ri, matr_copy_from_ri};
use tensors::matrix_blas_lapack::_dgemm;
use std::fmt::format;
use std::fs;
use std::sync::mpsc::channel;
use std::thread::panicking;
use crate::basis_io::etb::{get_etb_elem, etb_gen_for_atom_list, InfoV2};
use crate::constants::{ELEM1ST, ELEM2ND, ELEM3RD, ELEM4TH, ELEM5TH,ELEM6TH, ELEMTMS};
use crate::dft::DFA4REST;
use crate::geom_io::{GeomCell,MOrC, GeomUnit, get_mass_charge};
use crate::basis_io::{Basis4Elem,BasInfo};
use crate::ctrl_io::{InputKeywords};
use crate::utilities;
use rust_libcint::{CINTR2CDATA, CintType};
use std::path::Path;
use regex::Regex;
use crate::basis_io::bse_downloader::{self, basis_modifier};
use crate::basis_io::basis_list::{self, basis_fuzzy_matcher, check_basis_name};

//extern crate nalgebra as na;
//use na::{DMatrix,DVector};
//use crate::geom_io::{GeomCell,GeomCell,CodeSelect,MOrC, GeomUnit, RawGeomCell};

pub fn get_basis_name(ang: usize, ctype: &CintType, index: usize) -> String {
    let mut ang_name = if ang==0 {String::from("S")
    } else if ang==1 {String::from("P")
    } else if ang==2 {String::from("D")
    } else if ang==3 {String::from("F")
    } else if ang==4 {String::from("G")
    } else if ang==5 {String::from("H")
    } else if ang==6 {String::from("I")
    } else {
        panic!("Error:: the GTO basis function with angular momentum larger than 6 is not yet supported");
    };
    match ctype {
        CintType::Spheric => {ang_name = format!("{}-{}",ang_name, index)},
        CintType::Cartesian => {ang_name = format!("{}-{}",ang_name, index)}, 
    };
    ang_name
}

#[derive(Clone)]
/// # Molecule
/// `Molecule` contains all basic information of the molecule in the calculation task
/// self.ctrl:         include the input keywords and many derived keywors to control the calculation task
/// self.bas:          the basis set shell by shell organized for libcint
/// self.geom:         the molecular structure information
/// self.xc_data:      the information and operations for DFT: [`DFA4REST`](DFA4REST).
/// 
/// self.spin_channel: 1: spin unpolarized or 2: spin polarized
/// self.num_state:    the number of molecular orbitals
/// self.num_basis:    the number of basis functions
/// self.start_mo:     the first molecular orbital in the frozen-core approximation for pt2, rpa and so forth
/// 
/// self.fdqc_bas:     the basis set information [`BasInfo`](BasInfo) for each shell
/// self.cint_*:       all these fields are the interface to ```libcint```
/// self.cint_type:    CintType::Spheric or CintType::Cartesian
pub struct Molecule {
    pub ctrl: InputKeywords,
    //pub bas : Vec<Basis4Elem>,
    //pub auxbas : Vec<Basis4Elem>,
    pub geom : GeomCell,
    pub spin_channel: usize,
    // exchange-correlation functionals
    pub xc_data: DFA4REST,
    pub num_state: usize,
    pub num_basis: usize,
    pub num_auxbas: usize,
    pub num_elec : Vec<f64>,
    // for frozen-core pt2, rpa and so forth
    pub start_mo : usize,
    pub basis4elem: Vec<Basis4Elem>,
    // fdqc_bas: store information of each basis functions
    pub fdqc_bas : Vec<BasInfo>,
    //  cint_fdqc: vec![[start of a basis shell, num of basis funciton in this shell]; num of shells]
    pub cint_fdqc: Vec<Vec<usize>>,
    //  cint_bas : vec![data for each basis shell; num of shells]
    //             data for each basis shell contains 8 slots, which are organized in the order of "bas" required by libcint
    pub cint_bas : Vec<Vec<i32>>,
    //  vec![data for each atom; num of atoms]
    //  data for each atom contains 6 slots, which are organized in the order of "atm" required by libcint
    pub cint_atm : Vec<Vec<i32>>,
    //  save the value of coordinates, exponents, contraction coefficients in the order of "env" required by libcint
    pub cint_env : Vec<f64>,
    pub fdqc_aux_bas : Vec<BasInfo>,
    pub cint_aux_fdqc: Vec<Vec<usize>>,
    pub cint_aux_bas : Vec<Vec<i32>>,
    pub cint_aux_atm : Vec<Vec<i32>>,
    pub cint_aux_env : Vec<f64>,
    pub cint_type: CintType,
    //    cint_data : CINTR2CDATA,
}

impl Molecule {
    pub fn new() -> Molecule {
        Molecule {
            ctrl:InputKeywords::new(),
            xc_data: DFA4REST::new("hf",1),
            geom: GeomCell::new(),
            num_elec: vec![0.0,0.0,0.0],
            num_state: 0,
            num_basis: 0,
            num_auxbas: 0,
            start_mo: 0,   // all-electron pt2, rpa and so forth
            spin_channel: 1,
            basis4elem: vec![],
            fdqc_bas: vec![],
            cint_bas: vec![],
            cint_fdqc: vec![],
            cint_atm: vec![],
            cint_env: vec![],
            fdqc_aux_bas: vec![],
            cint_aux_bas: vec![],
            cint_aux_fdqc: vec![],
            cint_aux_atm: vec![],
            cint_aux_env: vec![],
            cint_type: CintType::Spheric,
            //cint_data: CINTR2CDATA::new()
        }
    }
    pub fn build(ctrl_file: String) -> anyhow::Result<Molecule> {
        //let mut mol = Molecule::new();
        //let (mut ctrl, mut geom) = RawCtrl::parse_ctl_from_jsonfile_v02(ctrl_file)?;
        let (mut ctrl, mut geom) = InputKeywords::parse_ctl(ctrl_file)?;

        let cint_type = if ctrl.basis_type.to_lowercase()==String::from("spheric") {
            CintType::Spheric
        } else if ctrl.basis_type.to_lowercase()==String::from("cartesian") {
            CintType::Cartesian
        } else {
            panic!("Error:: Unknown basis type '{}'. Please use either 'spheric' or 'Cartesian'", 
                   ctrl.basis_type);
        };
        let (mut bas,mut cint_atm,mut cint_bas,cint_env,
            fdqc_bas,cint_fdqc,num_elec,num_basis,num_state) 
            = Molecule::collect_basis(&mut ctrl, &mut geom);
        let env = cint_env.clone();

        let natm = cint_atm.len() as i32;
        let nbas = cint_bas.len() as i32;
        println!("nbas: {}, natm: {} for standard basis sets", nbas, natm);
        
        let spin_channel = ctrl.spin_channel;

        let (mut auxbas , mut cint_aux_atm,mut cint_aux_bas,cint_aux_env,
                mut fdqc_aux_bas,mut cint_aux_fdqc,num_auxbas) 
            =(bas.clone(),Vec::new(),Vec::new(),Vec::new(),Vec::new(),Vec::new(),0);


        let mut cint_data = CINTR2CDATA::new();
        cint_data.set_cint_type(&cint_type);
        cint_data.initial_r2c(&cint_atm, natm, &cint_bas, nbas, &env);

        //fdqc_aux_bas.iter().for_each(|i| {
        //    println!("{}", i.formated_name());
        //});

        let basis4elem = bas;

        let xc_data = DFA4REST::new(&ctrl.xc,spin_channel);

        xc_data.xc_version();

        // frozen-core pt2 and rpa are not yet implemented.
        let start_mo = count_frozen_core_states(ctrl.frozen_core_postscf, &geom.elem);

        Ok(Molecule {
            ctrl,
            geom,
            xc_data,
            num_elec,
            num_state,
            num_basis,
            num_auxbas,
            start_mo,
            spin_channel,
            basis4elem,
            fdqc_bas,
            cint_fdqc,
            cint_atm,
            cint_bas,
            cint_env,
            fdqc_aux_bas,
            cint_aux_fdqc,
            cint_aux_atm,
            cint_aux_bas,
            cint_aux_env,
            cint_type
        })
    }
    pub fn initialize_auxbas(&mut self) {
        let cint_type = if self.ctrl.basis_type.to_lowercase()==String::from("spheric") {
            CintType::Spheric
        } else if self.ctrl.basis_type.to_lowercase()==String::from("cartesian") {
            CintType::Cartesian
        } else {
            panic!("Error:: Unknown basis type '{}'. Please use either 'spheric' or 'Cartesian'", 
                   self.ctrl.basis_type);
        };


        let etb = if (self.ctrl.even_tempered_basis != String::from("none")) {
            let etb_elem = get_etb_elem(&self.geom, &self.ctrl.etb_start_atom_number);
            let etb_basis = etb_gen_for_atom_list(&self, &self.ctrl.etb_beta, &etb_elem);
            Some(etb_basis)
            }
            else{
                None
            };


        // at first, deallocate the cint data for standard basis set
        //self.cint_data.final_c2r();
        // loading the auxiliary basis sets
        let (mut auxbas,mut cint_aux_atm,mut cint_aux_bas,cint_aux_env,
                mut fdqc_aux_bas,mut cint_aux_fdqc,num_auxbas) 
            = if self.ctrl.use_auxbas {
                Molecule::collect_auxbas(&mut self.ctrl, &mut self.geom, etb)
        } else {
            (Vec::new(),Vec::new(),Vec::new(),Vec::new(),Vec::new(),Vec::new(),0)
        };

        self.cint_aux_atm = cint_aux_atm;
        self.cint_aux_bas = cint_aux_bas;
        self.cint_aux_env = cint_aux_env;
        self.fdqc_aux_bas = fdqc_aux_bas;
        self.cint_aux_fdqc = cint_aux_fdqc;
        self.num_auxbas = num_auxbas;

        let mut env = self.cint_env.clone();

        println!("num_basis: {},num_auxbas: {}", self.num_basis,num_auxbas);
        let off = env.len() as i32;
        let natm_off = self.cint_atm.len() as i32;
        let nbas_off = self.cint_bas.len() as i32;
        // append auxliary basis set list into the standard basis set list for libcint
        self.cint_aux_atm.iter_mut().for_each(|i| {
            // offset the position that store coordinates in the env list
            if let Some(j) = i.get_mut(1) {*j += off};
            // offset the position that store nuclear charge distribution parameters in the env list
            if let Some(j) = i.get_mut(3) {*j += off};
        });
        self.cint_aux_bas.iter_mut().for_each(|i| {
            // offset the atom index in the atm list
            if let Some(j) = i.get_mut(0) {*j += natm_off};
            // offset the position that stores basis exponents in the env list
            if let Some(j) = i.get_mut(5) {*j += off};
            // offset the position that stores basis coefficients in the env list
            if let Some(j) = i.get_mut(6) {*j += off};
        });

        self.fdqc_aux_bas.iter_mut().for_each(|i| {i.cint_index0 += nbas_off as usize});

        //println!("final nbas: {},final natm: {}", nbas, natm);
        println!("final nbas: {},final natm: {}", self.cint_bas.len()+self.cint_aux_bas.len(), self.cint_atm.len()+self.cint_aux_atm.len());

    }

    pub fn initialize_cint(&self) -> CINTR2CDATA {
        let mut final_cint_atm = self.cint_atm.clone();
        final_cint_atm.extend(self.cint_aux_atm.clone());
        let mut final_cint_bas = self.cint_bas.clone();
        final_cint_bas.extend(self.cint_aux_bas.clone());
        let mut final_cint_env = self.cint_env.clone();
        final_cint_env.extend(self.cint_aux_env.clone());

        let natm = final_cint_atm.len() as i32;
        let nbas = final_cint_bas.len() as i32;
        let mut cint_data = CINTR2CDATA::new();
        cint_data.set_cint_type(&self.cint_type);
        cint_data.initial_r2c(&final_cint_atm, natm, &final_cint_bas, nbas, &final_cint_env);

        cint_data
    }

    pub fn collect_auxbas(ctrl: &InputKeywords,geom: &mut GeomCell, etb: Option<InfoV2>) -> 
            (Vec<Basis4Elem>, Vec<Vec<i32>>, Vec<Vec<i32>>, Vec<f64>, Vec<BasInfo>, Vec<Vec<usize>>, usize) {

        let mut aux_atm: Vec<Vec<i32>> = vec![];
        let mut aux_env: Vec<f64> = vec![];
        let mut geom_start: i32 = 0;
        let cint_type = if ctrl.basis_type.to_lowercase()==String::from("spheric") {
            CintType::Spheric
        } else if ctrl.basis_type.to_lowercase()==String::from("cartesian") {
            CintType::Cartesian
        } else {
            panic!("Error:: Unknown basis type '{}'. Please use either 'spheric' or 'Cartesian'", 
                   ctrl.basis_type);
        };

        //let (elem_name, elem_charge, elem_mass) = elements();
        let mass_charge = get_mass_charge(&geom.elem);

        geom.elem.iter().enumerate().zip(mass_charge.iter())
            .for_each(|((atm_index,atm_elem),(tmp_mass,tmp_charge))| {
            aux_atm.push(vec![*tmp_charge as i32,geom_start,1,geom_start+3,0,0]);
            (0..3).into_iter().for_each(|i| {
                if let Some(tmp_value) = geom.position.get(&[i,atm_index]) {
                    //for the coordinates
                    aux_env.push(*tmp_value);
                }
            });
            //for the nuclear charge distribution parameter
            aux_env.push(0.0);
            geom_start += 4;
        });
        //for (atm_index, atm_elem) in geom.elem.iter().enumerate() {
        //    let mut tmp_charge: i32 = 0;
        //    let tmp_item = elem_name.iter()
        //        .zip(elem_charge.iter()).find(|(x,y)| {x.eq(&atm_elem)});
        //    if let Some((x,y)) = tmp_item {
        //        tmp_charge = *y;
        //    };
        //    aux_atm.push(vec![tmp_charge,geom_start,1,geom_start+3,0,0]);
        //    (0..3).into_iter().for_each(|i| {
        //        if let Some(tmp_value) = geom.position.get(&[i,atm_index]) {
        //            //for the coordinates
        //            aux_env.push(*tmp_value);
        //        }
        //    });
        //    //for the nuclear charge distribution parameter
        //    aux_env.push(0.0);
        //    geom_start += 4;
        //}; 
        // Now for bas inf.
        let mut auxbas_total: Vec<Basis4Elem> = vec![];
        let mut aux_bas: Vec<Vec<i32>> = vec![];
        let mut basis_start = geom_start;
        let mut auxbas_info: Vec<BasInfo> = vec![];
        let mut aux_cint_fdqc: Vec<Vec<usize>> = vec![];

/*         let etb_basis_info = match etb {
            Some(x) => x,
            None => InfoV2::new(),
        };
 */
        //bse_auxbas_getter insert here
        let re = Regex::new(r"/{1}[^/]*$").unwrap();
        let cap = re.captures(&ctrl.auxbas_path).unwrap();
        let auxbas_name = cap[0][1..].to_string();
        if check_basis_name(&auxbas_name) {
            bse_downloader::bse_auxbas_getter(&auxbas_name,&geom, &ctrl.auxbas_path);
        }
        else {
            let matched = basis_fuzzy_matcher(&auxbas_name);
            match matched {
                Some(_) => panic!("{} may not be a valid basis set name, similar name is {}", auxbas_name, matched.unwrap()),
                None => panic!("{} may not be a valid basis set name, please check.", auxbas_name),
            }
        }

        for (atm_index, atm_elem) in geom.elem.iter().enumerate() {
            let tmp_path = format!("{}/{}.json",&ctrl.auxbas_path, &atm_elem);
            let mut tmp_basis = match &etb {
                None => Basis4Elem::parse_json_from_file(tmp_path,&cint_type).unwrap(),
                Some(x) => { match x.elements.get(atm_elem) {
                    Some(y) => y.clone(),
                    None => Basis4Elem::parse_json_from_file(tmp_path,&cint_type).unwrap(),
                }},
            };
           

            let mut num_basis_per_atm = 0_usize;
            for tmp_bascell in &tmp_basis.electron_shells {
                let mut num_primitive: i32 = tmp_bascell.exponents.len() as i32;
                let mut num_contracted: i32 = tmp_bascell.coefficients.len() as i32;
                let mut angular_mom: i32 = tmp_bascell.angular_momentum[0];
                let tmp_bas_info = BasInfo::new();
                aux_env.extend(tmp_bascell.exponents.clone());
                for (index, coe_vec) in tmp_bascell.coefficients.iter().enumerate() {
                    aux_env.extend(coe_vec.clone());
                };
                let mut tmp_bas_vec: Vec<i32> = vec![atm_index as i32, 
                            angular_mom,
                            num_primitive,
                            num_contracted,
                            0,
                            basis_start,
                            basis_start+num_primitive,
                            0];
                let (ang,tmp_bas_num) = match &cint_type {
                    CintType::Cartesian => {let ang = tmp_bas_vec[1] as usize; (ang,(ang+1)*(ang+2)/2)},
                    CintType::Spheric => {let ang = tmp_bas_vec[1] as usize; (ang, ang*2+1)},
                };
                let mut tmp_len = 0;
                let tmp_start = if aux_cint_fdqc.len()==0 {0} 
                                    else {aux_cint_fdqc[aux_cint_fdqc.len()-1][0]+aux_cint_fdqc[aux_cint_fdqc.len()-1][1]};
                //Now for bas info. of each basis function and their link to the libcint data structure
                (0..num_contracted as usize).into_iter().for_each(|index0| {
                    (0..tmp_bas_num).into_iter().for_each(|index1| {
                        let bas_type = if num_primitive == 1 {
                            String::from("Primitive")
                        } else {
                            String::from("Contracted")
                        };
                        tmp_len += 1;
                        auxbas_info.push(BasInfo {
                            bas_name: get_basis_name(ang, &cint_type, index1),
                            bas_type,
                            elem_index0: atm_index,
                            cint_index0: aux_bas.len(),
                            cint_index1: index0*tmp_bas_num+index1,
                        })
                    });
                });
                aux_cint_fdqc.push(vec![tmp_start,tmp_len]);
                aux_bas.push(tmp_bas_vec);
                basis_start += num_primitive + num_primitive*num_contracted;
                num_basis_per_atm += tmp_len 
            }

            auxbas_total.push(tmp_basis);
            if atm_index !=0 {
                auxbas_total[atm_index].global_index.0 = auxbas_total[atm_index-1].global_index.0 + auxbas_total[atm_index-1].global_index.1; 
                auxbas_total[atm_index].global_index.1 = num_basis_per_atm;
            } else {
                auxbas_total[atm_index].global_index.0 = 0;
                auxbas_total[atm_index].global_index.1 = num_basis_per_atm;
            }
        };
        // At current stage, we skip the linear-dependence check of the basis sets
        let num_auxbas = auxbas_info.len();

        (auxbas_total, aux_atm, aux_bas, aux_env,auxbas_info,aux_cint_fdqc,num_auxbas)
    }

    pub fn collect_basis(ctrl: &InputKeywords,geom: &mut GeomCell) -> 
            (Vec<Basis4Elem>, Vec<Vec<i32>>, Vec<Vec<i32>>, Vec<f64>, Vec<BasInfo>, Vec<Vec<usize>>, Vec<f64>,usize, usize) {
        //let (elem_name, elem_charge, elem_mass) = elements();
        let mass_charge = get_mass_charge(&geom.elem);
        let mut atm: Vec<Vec<i32>> = vec![];
        let mut env: Vec<f64> = vec![];
        let mut geom_start: i32 = 0;

        let cint_type = if ctrl.basis_type.to_lowercase()==String::from("spheric") {
            CintType::Spheric
        } else if ctrl.basis_type.to_lowercase()==String::from("cartesian") {
            CintType::Cartesian
        } else {
            panic!("Error:: Unknown basis type '{}'. Please use either 'spheric' or 'Cartesian'", 
                   ctrl.basis_type);
        };

        // Prepare atm info.
        let  mut num_elec = vec![0.0,0.0,0.0];
        geom.elem.iter().enumerate().zip(mass_charge.iter())
            .for_each(|((atm_index,atm_elem),(tmp_mass,tmp_charge))| {
            num_elec[0] += tmp_charge;
            atm.push(vec![*tmp_charge as i32,geom_start,1,geom_start+3,0,0]);
            (0..3).into_iter().for_each(|i| {
                if let Some(tmp_value) = geom.position.get(&[i,atm_index]) {
                    //for the coordinates
                    env.push(*tmp_value);
                }
            });
            //for the nuclear charge distribution parameter
            env.push(0.0);
            geom_start += 4;
        });

        /*// Prepare atm info.
        let  mut num_elec = vec![0.0,0.0,0.0];
        for (atm_index, atm_elem) in geom.elem.iter().enumerate() {
            let mut tmp_charge: i32 = 0;
            let tmp_item = elem_name.iter()
                .zip(elem_charge.iter()).find(|(x,y)| {x.eq(&atm_elem)});
            if let Some((x,y)) = tmp_item {
                tmp_charge = *y;
                num_elec[0] += tmp_charge as f64;
            };
            atm.push(vec![tmp_charge,geom_start,1,geom_start+3,0,0]);
            (0..3).into_iter().for_each(|i| {
                if let Some(tmp_value) = geom.position.get(&[i,atm_index]) {
                    // for the coordinate
                    env.push(*tmp_value);
                }
            });
            // for the nuclear charge distribution parameter
            env.push(0.0);
            geom_start += 4;
        };*/ 

        // Now for bas inf.
        let mut basis_total: Vec<Basis4Elem> = vec![];
        let mut bas: Vec<Vec<i32>> = vec![];
        let mut basis_start = geom_start;
        let mut bas_info: Vec<BasInfo> = vec![];
        let mut cint_fdqc: Vec<Vec<usize>> = vec![];
        //let mut basis_global_index: Vec<(usize,usize)> = vec![(0,0);geom.elem.len()];

        //function inserted here
        let re = Regex::new(r"/{1}[^/]*$").unwrap();
        let cap = re.captures(&ctrl.basis_path).unwrap();
        let basis_name = cap[0][1..].to_string();
        if check_basis_name(&basis_name) {
            bse_downloader::bse_basis_getter(&basis_name,&geom, &ctrl.basis_path);
        }
        else {
            let matched = basis_fuzzy_matcher(&basis_name);
            match matched {
                Some(_) => panic!("{} may not be a valid basis set name, similar name is {}", basis_name, matched.unwrap()),
                None => panic!("{} may not be a valid basis set name, please check.", basis_name),
            }
        }
        //bse_downloader::bse_basis_getter(&basis_name,&geom, &ctrl.basis_path);


        for (atm_index, atm_elem) in geom.elem.iter().enumerate() {
            //if atm_index !=0 {
            //    basis_global_index[atm_index].0 = basis_global_index[atm_index-1].0+basis_global_index[atm_index-1].1;
            //    basis_global_index[atm_index].1 = 0;
            //} else {
            //    basis_global_index[atm_index].0 = 0;
            //    basis_global_index[atm_index].1 = 0;
            //}
            let tmp_path = format!("{}/{}.json",&ctrl.basis_path, &atm_elem);
            let mut tmp_basis = Basis4Elem::parse_json_from_file(tmp_path,&cint_type).unwrap();
/* 
                if Path::new(&tmp_path).exists() {
                    Basis4Elem::parse_json_from_file(tmp_path,&cint_type).unwrap()
                }
                else {

                    let re = Regex::new(r"/{1}[^/]*$").unwrap();
                    let cap = re.captures(&ctrl.basis_path).unwrap();
                    let basis_name = cap[0][1..].to_string();
                    println!("No local {} for {}, trying to download from BasisSetExchange.org...", basis_name, atm_elem);
                    bse_downloader::bse_basis_getter(&basis_name,&geom, &ctrl.basis_path);
                    basis_modifier(&tmp_path);
                    Basis4Elem::parse_json_from_file(tmp_path,&cint_type).unwrap()        
                };
 */
            let mut num_basis_per_atm = 0_usize;
            for tmp_bascell in &tmp_basis.electron_shells {
                let mut num_primitive: i32 = tmp_bascell.exponents.len() as i32;
                let mut num_contracted: i32 = tmp_bascell.coefficients.len() as i32;
                let mut angular_mom: i32 = tmp_bascell.angular_momentum[0];
                let tmp_bas_info = BasInfo::new();
                tmp_bascell.exponents.iter().for_each(|x| {
                    env.push(*x);
                });
                for (index, coe_vec) in tmp_bascell.coefficients.iter().enumerate() {
                    coe_vec.iter().for_each(|x| {
                        env.push(*x);
                    });
                };
                let mut tmp_bas_vec: Vec<i32> = vec![atm_index as i32, 
                            angular_mom,
                            num_primitive,
                            num_contracted,
                            0,
                            basis_start,
                            basis_start+num_primitive,
                            0];
                let (ang,tmp_bas_num) = match &cint_type {
                    CintType::Cartesian => {let ang = tmp_bas_vec[1] as usize; (ang,(ang+1)*(ang+2)/2)},
                    CintType::Spheric => {let ang = tmp_bas_vec[1] as usize; (ang, ang*2+1)},
                };
                let mut tmp_len = 0;
                let tmp_start = if cint_fdqc.len()==0 {0} 
                                    else {cint_fdqc[cint_fdqc.len()-1][0]+cint_fdqc[cint_fdqc.len()-1][1]};
                //Now for bas info. of each basis function and their link to the libcint data structure
                (0..num_contracted as usize).into_iter().for_each(|index0| {
                    (0..tmp_bas_num).into_iter().for_each(|index1| {
                        let bas_type = if num_primitive == 1 {
                            String::from("Primitive")
                        } else {
                            String::from("Contracted")
                        };
                        tmp_len += 1;
                        bas_info.push(BasInfo {
                            bas_name: get_basis_name(ang, &cint_type, index1),
                            bas_type,
                            elem_index0: atm_index,
                            cint_index0: bas.len(),
                            cint_index1: index0*tmp_bas_num+index1,
                        })
                    });
                });
                cint_fdqc.push(vec![tmp_start,tmp_len]);
                bas.push(tmp_bas_vec);
                basis_start += num_primitive + num_primitive*num_contracted;
                num_basis_per_atm += tmp_len;
            }

            basis_total.push(tmp_basis);

            if atm_index !=0 {
                basis_total[atm_index].global_index.0 = basis_total[atm_index-1].global_index.0 + basis_total[atm_index-1].global_index.1; 
                basis_total[atm_index].global_index.1 = num_basis_per_atm;
            } else {
                basis_total[atm_index].global_index.0 = 0;
                basis_total[atm_index].global_index.1 = num_basis_per_atm;
            }
        };

        // determine the electron number in total and in each spin channel.
        num_elec[0]-=ctrl.charge;
        let unpair_elec = (ctrl.spin-1.0_f64);
        num_elec[1] = (num_elec[0]-unpair_elec)/2.0 + unpair_elec;
        num_elec[2] = (num_elec[0]-unpair_elec)/2.0;

        let num_basis = bas_info.len();
        // At current stage, we skip the linear-dependence check of the basis sets
        let num_state = num_basis;

        (basis_total, atm, bas, env,bas_info,cint_fdqc,num_elec,num_basis, num_state)
    }

    #[inline]
    pub fn int_ij_matrixupper(&mut self,op_name: String) -> MatrixUpper<f64> {
        let mut cur_op = op_name.clone();
        let mut cint_data = self.initialize_cint();
        if op_name == String::from("ovlp") {
            cint_data.cint1e_ovlp_optimizer_rust();
        } else if op_name == String::from("kinetic") {
            cint_data.cint1e_kin_optimizer_rust();
        } else if op_name == String::from("hcore") {
            cint_data.cint1e_kin_optimizer_rust();
            cur_op = String::from("kinetic");
        } else if op_name == String::from("nuclear") {
            cint_data.cint1e_nuc_optimizer_rust();
        } else {
            panic!("Error:: unknown operator for GTO-ij integrals: {}",&op_name);
        };
        let nbas = self.fdqc_bas.len();
        let tmp_size:usize = nbas*(nbas+1)/2;
        let mut mat_global = MatrixUpper::new(tmp_size,0.0);
        let nbas_shell = self.cint_bas.len();
        for j in 0..nbas_shell {
            let bas_start_j = self.cint_fdqc[j][0];
            let bas_len_j = self.cint_fdqc[j][1];
            // for i < j
            for i in 0..j {
                let bas_start_i = self.cint_fdqc[i][0];
                let bas_len_i = self.cint_fdqc[i][1];
                let tmp_size = [bas_len_i,bas_len_j];
                let mat_local = MatrixFull::from_vec(tmp_size,
                    cint_data.cint_ij(i as i32, j as i32, &cur_op)).unwrap();
                (0..bas_len_j).into_iter().for_each(|tmp_j| {
                    let gj = tmp_j + bas_start_j;
                    let global_ij_start = gj*(gj+1)/2+bas_start_i;
                    let local_ij_start = tmp_j*bas_len_i;
                    //let length = if bas_start_i+bas_len_i <= gj+1 {bas_len_i} else {gj+1-bas_start_i};
                    let length = bas_len_i;
                    let mat_global_j = mat_global.get1d_slice_mut(global_ij_start,length).unwrap();
                    let mat_local_j = mat_local.get1d_slice(local_ij_start,length).unwrap();
                    mat_global_j.iter_mut().zip(mat_local_j.iter()).for_each(|(gij,lij)| {
                        *gij = *lij
                    });
                });
            };
            // for i = j 
            let tmp_size = [bas_len_j,bas_len_j];
            let mat_local = MatrixFull::from_vec(tmp_size,
                cint_data.cint_ij(j as i32, j as i32, &cur_op)).unwrap();
            (0..bas_len_j).into_iter().for_each(|tmp_j| {
                let gj = bas_start_j + tmp_j;
                let global_ij_start = gj*(gj+1)/2+bas_start_j;
                let local_ij_start = tmp_j*bas_len_j;
                let length = tmp_j + 1;
                let mat_global_j = mat_global.get1d_slice_mut(global_ij_start,length).unwrap();
                let mat_local_j = mat_local.get1d_slice(local_ij_start,length).unwrap();
                mat_global_j.iter_mut().zip(mat_local_j.iter()).for_each(|(gij,lij)| {
                    *gij = *lij
                });
            });
        }
        if op_name == String::from("hcore") {
            //println!("Debug: The Kinetic matrix:");
            //mat_global.formated_output(5, "lower".to_string());
            cint_data.cint_del_optimizer_rust();
            cint_data.cint1e_nuc_optimizer_rust();
            cur_op = String::from("nuclear");
            for j in 0..nbas_shell {
                let bas_start_j = self.cint_fdqc[j][0];
                let bas_len_j = self.cint_fdqc[j][1];
                // for i < j
                for i in 0..j {
                    let bas_start_i = self.cint_fdqc[i][0];
                    let bas_len_i = self.cint_fdqc[i][1];
                    let tmp_size = [bas_len_i,bas_len_j];
                    let mat_local = MatrixFull::from_vec(tmp_size,
                        cint_data.cint_ij(i as i32, j as i32, &cur_op)).unwrap();
                    (0..bas_len_j).into_iter().for_each(|tmp_j| {
                        let gj = tmp_j + bas_start_j;
                        let global_ij_start = gj*(gj+1)/2+bas_start_i;
                        let local_ij_start = tmp_j*bas_len_i;
                        //let length = if bas_start_i+bas_len_i <= gj+1 {bas_len_i} else {gj+1-bas_start_i};
                        let length = bas_len_i;
                        //println!("debug: global_ij_start:{}, length: {}",global_ij_start, length);
                        let mat_global_j = mat_global.get1d_slice_mut(global_ij_start,length).unwrap();
                        let mat_local_j = mat_local.get1d_slice(local_ij_start,length).unwrap();
                        mat_global_j.iter_mut().zip(mat_local_j.iter()).for_each(|(gij,lij)| {
                            *gij += *lij
                        });
                    });
                };
                // for i = j 
                let tmp_size = [bas_len_j,bas_len_j];
                let mat_local = MatrixFull::from_vec(tmp_size,
                    cint_data.cint_ij(j as i32, j as i32, &cur_op)).unwrap();
                //if j==0 {println!("debug: I:{}, bas_len_j:{} cur_op: {}, {:?}", j,bas_start_j, &cur_op, &mat_local.data)};
                (0..bas_len_j).into_iter().for_each(|tmp_j| {
                    let gj = bas_start_j + tmp_j;
                    let global_ij_start = gj*(gj+1)/2+bas_start_j;
                    let local_ij_start = tmp_j*bas_len_j;
                    let length = tmp_j + 1;
                    //println!("debug: global_ij_start:{}, length: {}",global_ij_start, length);
                    let mat_global_j = mat_global.get1d_slice_mut(global_ij_start,length).unwrap();
                    let mat_local_j = mat_local.get1d_slice(local_ij_start,length).unwrap();
                    mat_global_j.iter_mut().zip(mat_local_j.iter()).for_each(|(gij,lij)| {
                        *gij += *lij
                    });
                });
            }
            //println!("The h-core matrix:");
            //mat_global.formated_output(5, "lower".to_string());
        }
        cint_data.final_c2r();
        //vec_2d
        mat_global
    }
    #[inline]
    pub fn int_ijkl_erifull(&mut self) -> ERIFull<f64> {
        //let mut dt_cint = 0.0_f64;
        //let dt1 = time::Local::now();
        let mut cint_data = self.initialize_cint();
        let nbas = self.num_basis;
        let mut mat_full = 
            ERIFull::new([nbas,nbas,nbas,nbas],0.0);
        let nbas_shell = self.cint_bas.len();
        cint_data.cint2e_optimizer_rust();
        for l in 0..nbas_shell {
            let bas_start_l = self.cint_fdqc[l][0];
            let bas_len_l = self.cint_fdqc[l][1];
            for k in 0..(l+1) {
                let bas_start_k = self.cint_fdqc[k][0];
                let bas_len_k = self.cint_fdqc[k][1];
                for j in 0..nbas_shell {
                    let bas_start_j = self.cint_fdqc[j][0];
                    let bas_len_j = self.cint_fdqc[j][1];
                    //let (i_start, i_end) = (0,j+1);
                    for i in 0..j+1 {
                        let bas_start_i = self.cint_fdqc[i][0];
                        let bas_len_i = self.cint_fdqc[i][1];
                        let buf = cint_data.cint_ijkl_by_shell(i as i32, j as i32, k as i32, l as i32);
                        //let dt_cint_0 = time::Local::now();
                        //let dt_cint_1 = time::Local::now();
                        mat_full.chrunk_copy([bas_start_i..bas_start_i+bas_len_i,
                                              bas_start_j..bas_start_j+bas_len_j,
                                              bas_start_k..bas_start_k+bas_len_k,
                                              bas_start_l..bas_start_l+bas_len_l,
                                              ], buf.clone());
                        // copy the "upper" part to the lower part
                        if i<j {
                            mat_full.chrunk_copy_transpose_ij([
                                bas_start_i..bas_start_i+bas_len_i,
                                bas_start_j..bas_start_j+bas_len_j,
                                bas_start_k..bas_start_k+bas_len_k,
                                bas_start_l..bas_start_l+bas_len_l,
                                ], buf);
                        }
                    };
                }
            }
        }
        cint_data.final_c2r();
        // to copy the upper part of the (k,l) pair to the lower block
        for k in 0..nbas {
            for l in 0..k {
                let from_slice =  mat_full.get4d_slice([0,0,l,k], mat_full.indicing[2]).unwrap().to_vec();
                let mut to_slice = mat_full.get4d_slice_mut([0,0,k,l], mat_full.indicing[2]).unwrap();
                //unsafe{
                //    dcopy(to_slice.len() as i32, &from_slice, 1, to_slice, 1);
                //}
                to_slice.iter_mut().zip(from_slice.iter()).for_each(|(t,f)|*t = *f);
            }
        }
        mat_full
    }
    #[inline]
    pub fn int_ijkl_erifold4(&mut self) -> ERIFold4<f64> {
        let mut cint_data = self.initialize_cint();
        //let mut dt_cint = 0.0_f64;
        //let dt1 = time::Local::now();
        let nbas = self.num_basis;
        let npair = nbas*(nbas+1)/2;
        let mut mat_full = ERIFold4::new([npair,npair],0.0);
        let nbas_shell = self.cint_bas.len();
        cint_data.cint2e_optimizer_rust();
        for l in 0..nbas_shell {
            let bas_start_l = self.cint_fdqc[l][0];
            let bas_len_l = self.cint_fdqc[l][1];
            for k in 0..(l+1) {
                let bas_start_k = self.cint_fdqc[k][0];
                let bas_len_k = self.cint_fdqc[k][1];
                for j in 0..nbas_shell {
                    let bas_start_j = self.cint_fdqc[j][0];
                    let bas_len_j = self.cint_fdqc[j][1];
                    let (i_start, i_end) = (0,j+1);
                    for i in 0..j+1 {
                        let bas_start_i = self.cint_fdqc[i][0];
                        let bas_len_i = self.cint_fdqc[i][1];
                        let buf = cint_data.cint_ijkl_by_shell(i as i32, j as i32, k as i32, l as i32);
                        mat_full.chunk_copy_from_local_erifull(nbas, 
                            bas_start_i..bas_start_i+bas_len_i,
                            bas_start_j..bas_start_j+bas_len_j,
                            bas_start_k..bas_start_k+bas_len_k,
                            bas_start_l..bas_start_l+bas_len_l,
                            buf);
                    }
                }
            }
        }
        cint_data.final_c2r();
        mat_full
    }
    pub fn int_ijkl_from_r3fn(&mut self, r3fn: &RIFull<f64>) -> ERIFold4<f64> {
        let nbas = self.num_basis;
        let npair = nbas*(nbas+1)/2;

        let mut mat_full = MatrixFull::new([npair,npair],0.0);
        let mut tmp_mat = MatrixFull::new([npair,1],0.0);
        &r3fn.data.chunks(self.num_basis*self.num_basis).for_each(|value| {
           value.iter().enumerate().filter(|value| value.0%nbas<=value.0/nbas)
           .map(|value| value.1)
           .zip(tmp_mat.data.iter_mut())
           .for_each(|value| {*value.1 = *value.0});
           let mut tmp_mat_b = MatrixFull::from_vec([npair,1],tmp_mat.data.clone()).unwrap();
           mat_full.lapack_dgemm(&mut tmp_mat, &mut tmp_mat_b, 'N', 'T', 1.0, 1.0);
        });
        ERIFold4::from_vec([npair,npair],mat_full.data).unwrap()

    }

    pub fn int_ijk_rifull(&mut self) -> RIFull<f64> {
        let mut cint_data = self.initialize_cint();
        // It is O_V = (ij|\nu)
        let n_basis = self.num_basis;
        let n_auxbas = self.num_auxbas;
        let mut ri3fn = RIFull::new([n_basis,n_basis,n_auxbas],0.0);
        let n_basis_shell = self.cint_bas.len();
        let n_auxbas_shell = self.cint_aux_bas.len();
        cint_data.cint3c2e_optimizer_rust();
        for k in 0..n_auxbas_shell {
            let basis_start_k = self.cint_aux_fdqc[k][0];
            let basis_len_k = self.cint_aux_fdqc[k][1];
            let gk  = k + n_basis_shell;
            for j in 0..n_basis_shell {
                let basis_start_j = self.cint_fdqc[j][0];
                let basis_len_j = self.cint_fdqc[j][1];
                // can be optimized with "for i in 0..(j+1)"
                for i in 0..n_basis_shell {
                    let basis_start_i = self.cint_fdqc[i][0];
                    let basis_len_i = self.cint_fdqc[i][1];
                    let buf = RIFull::from_vec([basis_len_i, basis_len_j,basis_len_k], 
                        cint_data.cint_3c2e(i as i32, j as i32, gk as i32)).unwrap();
                    ri3fn.copy_from_ri(
                        basis_start_i..basis_start_i+basis_len_i,
                        basis_start_j..basis_start_j+basis_len_j,
                        basis_start_k..basis_start_k+basis_len_k,
                        & buf, 
                        0..basis_len_i, 
                        0..basis_len_j, 
                        0..basis_len_k);
                    //let mut tmp_slices = ri3fn.get_slices_mut(
                    //    basis_start_i..basis_start_i+basis_len_i,
                    //    basis_start_j..basis_start_j+basis_len_j,
                    //    basis_start_k..basis_start_k+basis_len_k);
                    //tmp_slices.zip(buf.iter()).for_each(|value| {*value.0 = *value.1});
                }
            }
        }
        cint_data.final_c2r();
        ri3fn
    }

    pub fn int_ijk_rifull_rayon(&mut self) -> RIFull<f64> {
        // It is O_V = (ij|\nu)
        let n_basis = self.num_basis;
        let n_auxbas = self.num_auxbas;
        let mut ri3fn = RIFull::new([n_basis,n_basis,n_auxbas],0.0);
        let n_basis_shell = self.cint_bas.len();
        let n_auxbas_shell = self.cint_aux_bas.len();
        let cint_type = if self.ctrl.basis_type.to_lowercase()==String::from("spheric") {
            CintType::Spheric
        } else if self.ctrl.basis_type.to_lowercase()==String::from("cartesian") {
            CintType::Cartesian
        } else {
            panic!("Error:: Unknown basis type '{}'. Please use either 'spheric' or 'Cartesian'", 
                   self.ctrl.basis_type);
        };
        //let cint_fdqc = self.cint_fdqc.clone();
        //let mut final_cint_env = self.cint_env.clone();
        //final_cint_env.extend(self.cint_aux_env.clone());
        //let mut final_cint_atm = self.cint_atm.clone();
        //final_cint_atm.extend(self.cint_aux_atm.clone());
        //let mut final_cint_bas = self.cint_bas.clone();
        //final_cint_bas.extend(self.cint_aux_bas.clone());

        //let natm = final_cint_atm.len() as i32;
        //let nbas = final_cint_bas.len() as i32;


        let (sender, receiver) = channel();
        self.cint_aux_fdqc.par_iter().enumerate().for_each_with(sender,|s,(k,bas_info)| {

            // first, initialize rust_cint for each rayon threads
            let mut cint_data = self.initialize_cint();
            //let mut cint_data = CINTR2CDATA::new();
            //cint_data.set_cint_type(&cint_type);
            //cint_data.initial_r2c(&final_cint_atm, natm, &final_cint_bas, nbas, &final_cint_env);
            //cint_data.cint3c2e_optimizer_rust();

            let basis_start_k = bas_info[0];
            let basis_len_k = bas_info[1];
            let mut ri_rayon = RIFull::new([n_basis,n_basis,basis_len_k],1.0);
            let gk  = k + n_basis_shell;

            for j in 0..n_basis_shell {
                //let basis_start_j = cint_fdqc[j][0];
                //let basis_len_j = cint_fdqc[j][1];
                let basis_start_j = self.cint_fdqc[j][0];
                let basis_len_j = self.cint_fdqc[j][1];
                // can be optimized with "for i in 0..(j+1)"
                for i in 0..n_basis_shell {
                    //let basis_start_i = cint_fdqc[i][0];
                    //let basis_len_i = cint_fdqc[i][1];
                    let basis_start_i = self.cint_fdqc[i][0];
                    let basis_len_i = self.cint_fdqc[i][1];
                    let buf = RIFull::from_vec([basis_len_i, basis_len_j,basis_len_k], 
                        cint_data.cint_3c2e(i as i32, j as i32, gk as i32)).unwrap();
                    ri_rayon.copy_from_ri(
                        basis_start_i..basis_start_i+basis_len_i,
                        basis_start_j..basis_start_j+basis_len_j,
                        0..basis_len_k,
                        //basis_start_k..basis_start_k+basis_len_k,
                        & buf, 
                        0..basis_len_i, 
                        0..basis_len_j, 
                        0..basis_len_k);
                    //let mut tmp_slices = ri3fn.get_slices_mut(
                    //    basis_start_i..basis_start_i+basis_len_i,
                    //    basis_start_j..basis_start_j+basis_len_j,
                    //    basis_start_k..basis_start_k+basis_len_k);
                    //tmp_slices.zip(buf.iter()).for_each(|value| {*value.0 = *value.1});
                }
            }
            cint_data.final_c2r();


            s.send((ri_rayon,basis_start_k,basis_len_k)).unwrap()

        });

        receiver.into_iter().for_each(|(ri_rayon, basis_start_k,basis_len_k)| {
            ri3fn.copy_from_ri(
                0..n_basis,0..n_basis,basis_start_k..basis_start_k+basis_len_k,
                &ri_rayon,
                0..n_basis,0..n_basis,0..basis_len_k,
            );
        });

        ri3fn
    }

    pub fn int_ij_aux_columb(&mut self) -> MatrixFull<f64> {
        let mut cint_data = self.initialize_cint();
        let n_auxbas = self.num_auxbas;
        let n_basis_shell = self.cint_bas.len();
        let n_auxbas_shell = self.cint_aux_bas.len();
        cint_data.cint2c2e_optimizer_rust();
        let mut aux_v = MatrixFull::new([n_auxbas,n_auxbas],0.0);
        for l in 0..n_auxbas_shell {
            let basis_start_l = self.cint_aux_fdqc[l][0];
            let basis_len_l = self.cint_aux_fdqc[l][1];
            let gl  = l + n_basis_shell;
            for k in 0..n_auxbas_shell {
                let basis_start_k = self.cint_aux_fdqc[k][0];
                let basis_len_k = self.cint_aux_fdqc[k][1];
                let gk  = k + n_basis_shell;
                let buf = cint_data.cint_2c2e(gk as i32, gl as i32);
                //println!("debug 1 start with k: ({},{},{}), l: ({},{},{})", k,basis_start_k,basis_len_k, l,basis_start_l,basis_len_l);
                let mut tmp_slices = aux_v.iter_submatrix_mut(
                    basis_start_k..basis_start_k+basis_len_k,
                    basis_start_l..basis_start_l+basis_len_l);
                tmp_slices.zip(buf.iter()).for_each(|value| {*value.0 = *value.1});
                //println!("debug 1 end with k: {}, l: {}", l, k);

            }
        }
        cint_data.final_c2r();
        aux_v
    }

    pub fn prepare_ri3fn_for_ri_v(&mut self) -> RIFull<f64> {
        // prepare O_V * V^{-1/2}

        let mut time_records = utilities::TimeRecords::new();
        time_records.new_item("all ri", "for the total evaluations of ri3fn");
        time_records.count_start("all ri");

        time_records.new_item("prim ri", "for the pure three-center integrals");
        time_records.new_item("aux_ij", "for the coulomb matrix of auxiliary basis");
        time_records.new_item("sqrt_matr", "for inverse sqrt of the coulomb matrix of auxiliary basis");
        time_records.new_item("trans", "for O_V*V^{-1/2}");


        let n_basis = self.num_basis;
        let n_auxbas = self.num_auxbas;


        // at frist, prepare the 3-center integrals: O_V = (ij|\nu)
        time_records.count_start("prim ri");
        let mut ri3fn = self.int_ijk_rifull_rayon();
        time_records.count("prim ri");
        // then the auxiliary 2-center coulumb matrix: V=(\nu|\mu)
        time_records.count_start("aux_ij");
        let mut aux_v = self.int_ij_aux_columb();
        time_records.count("aux_ij");

        // we calculate the square roote of the inversion matrix: V^{-1/2}
        time_records.count_start("sqrt_matr");
        aux_v = aux_v.lapack_power(-0.5, 1.0E-6).unwrap();
        time_records.count("sqrt_matr");

        // println!("Print out {} 2-center auxiliary coulomb integral elements", tmp_num);

        // perform O_V*V^{-1/2}
        time_records.count_start("trans");
        let mut tmp_ovlp_matr = MatrixFull::new([n_basis,n_auxbas],0.0);
        let mut aux_ovlp_matr = MatrixFull::new([n_basis,n_auxbas],0.0);
        for j in 0..n_basis {
            tmp_ovlp_matr.data.iter_mut().zip(ri3fn.get_slices(0..n_basis,j..j+1,0..n_auxbas))
                .for_each(|value| {
                    //println!("debug: {:?}", value);
                    *value.0 = *value.1;
                });
            aux_ovlp_matr.lapack_dgemm(&mut tmp_ovlp_matr, &mut aux_v, 'N', 'N', 1.0,0.0);
            ri3fn.get_slices_mut(0..n_basis,j..j+1,0..n_auxbas).zip(aux_ovlp_matr.data.iter())
                .for_each(|value| {*value.0 = *value.1});
            //ri3fn.copy_from_matr(0..n_basis, 0..n_auxbas, j, 1, 
            //    &aux_ovlp_matr, 0..n_basis, 0..n_auxbas)
        }
        time_records.count("trans");

        time_records.count("all ri");

        if self.ctrl.print_level>=2 {
            time_records.report_all();
        }

        ri3fn
    }

    pub fn prepare_ri3fn_for_ri_v_rayon(&mut self) -> RIFull<f64> {
        // prepare O_V * V^{-1/2}

        let mut time_records = utilities::TimeRecords::new();
        time_records.new_item("all ri", "for the total evaluations of ri3fn");
        time_records.count_start("all ri");

        time_records.new_item("prim ri", "for the pure three-center integrals");
        time_records.new_item("aux_ij", "for the coulomb matrix of auxiliary basis");
        time_records.new_item("sqrt_matr", "for inverse sqrt of the coulomb matrix of auxiliary basis");
        time_records.new_item("trans", "for O_V*V^{-1/2}");


        let n_basis = self.num_basis;
        let n_auxbas = self.num_auxbas;


        // at frist, prepare the 3-center integrals: O_V = (ij|\nu)
        time_records.count_start("prim ri");
        let mut ri3fn = self.int_ijk_rifull_rayon();
        time_records.count("prim ri");
        // then the auxiliary 2-center coulumb matrix: V=(\nu|\mu)
        time_records.count_start("aux_ij");
        let mut aux_v = self.int_ij_aux_columb();
        time_records.count("aux_ij");

        time_records.count_start("sqrt_matr");
        //// we calculate the square roote of the inversion matrix: V^{-1/2}
        ////aux_v = aux_v.lapack_power(-0.5, 1.0E-6).unwrap();
        // we calculate the Cholesky decomposition L of the inversion matrix: L*L^{T}=V^{-1}
        aux_v = aux_v.to_matrixfullslicemut().cholesky_decompose_inverse('L').unwrap();
        time_records.count("sqrt_matr");

        // println!("Print out {} 2-center auxiliary coulomb integral elements", tmp_num);

        // perform O_V*V^{-1/2}
        time_records.count_start("trans");

        // 1) Version of Fortran wrapper
        //let ri3fn_size = ri3fn.size.clone();
        //rest_tensors::external_libs::special_dgemm_f_01(
        //    &mut ri3fn.data[..], &ri3fn_size, 0..n_basis, 0, 0..n_auxbas,
        //    aux_v.data_ref().unwrap(), aux_v.size(), 0..n_auxbas, 0..n_auxbas,
        //    1.0, 0.0
        //);

        let mut tmp_ovlp_matr = MatrixFull::new([n_basis,n_auxbas],0.0);
        let mut aux_ovlp_matr = MatrixFull::new([n_basis,n_auxbas],0.0);
        let size = [n_basis,n_auxbas];
        for j in 0..n_basis {
            // 2) Rayon Version
            //utilities::omp_set_num_threads(1);
            //let task_distribution = utilities::balancing(n_auxbas,rayon::current_num_threads()-1);
            ////let mut aux_ovlp_matr = MatrixFull::new([n_basis,n_auxbas],0.0);
            //let (sender, receiver) = channel();
            //task_distribution.par_iter().for_each_with(sender,|s,range_auxbas| {
            //    let mut loc_ovlp_matr = MatrixFull::new([n_basis,range_auxbas.len()],0.0);
            //    loc_ovlp_matr.data.iter_mut().zip(ri3fn.get_slices(0..n_basis,j..j+1,range_auxbas.clone()))
            //        .for_each(|value| {
            //            *value.0 = *value.1;
            //        });
            //    let mut aux_ovlp_matr = MatrixFull::new([n_basis,n_auxbas],0.0);
            //    _dgemm(&loc_ovlp_matr, (0..n_basis,0..range_auxbas.len()), 'N', 
            //        &aux_v, (range_auxbas.clone(), 0..n_auxbas), 'N', 
            //        &mut aux_ovlp_matr, (0..n_basis, 0..n_auxbas), 1.0, 0.0);
            //    s.send((aux_ovlp_matr,range_auxbas)).unwrap()
            //});
            //receiver.into_iter().enumerate().for_each(|(index, (loc_aux_ovlp_matr,range_auxbas))| {
            //    //aux_ovlp_matr += loc_aux_ovlp_matr;
            //    if index != 0 {
            //    ri3fn.get_slices_mut(0..n_basis,j..j+1,0..n_auxbas).zip(loc_aux_ovlp_matr.data.iter())
            //        .for_each(|value| {*value.0 += *value.1});
            //    } else {
            //    ri3fn.get_slices_mut(0..n_basis,j..j+1,0..n_auxbas).zip(loc_aux_ovlp_matr.data.iter())
            //        .for_each(|value| {*value.0 = *value.1});
            //    }
            //});
            //utilities::omp_set_num_threads(self.ctrl.num_threads.unwrap());

            // 3) the old version

            //tmp_ovlp_matr.data.iter_mut().zip(ri3fn.get_slices(0..n_basis,j..j+1,0..n_auxbas))
            //    .for_each(|value| {
            //        *value.0 = *value.1;
            //    });

            matr_copy_from_ri(&ri3fn.data, &ri3fn.size,0..n_basis, 0..n_auxbas, j, 1,
                &mut tmp_ovlp_matr.data, &size, 0..n_basis, 0..n_auxbas);

            aux_ovlp_matr.to_matrixfullslicemut().lapack_dgemm(
                &tmp_ovlp_matr.to_matrixfullslice(), &aux_v.to_matrixfullslice(), 
                'N', 'N', 1.0,0.0);

            //ri3fn.get_slices_mut(0..n_basis,j..j+1,0..n_auxbas).zip(aux_ovlp_matr.data.iter())
            //    .for_each(|value| {*value.0 = *value.1});
            ri3fn.copy_from_matr(0..n_basis, 0..n_auxbas, j, 1, 
                &aux_ovlp_matr, 0..n_basis, 0..n_auxbas)
        }
        
        time_records.count("trans");

        time_records.count("all ri");

        if self.ctrl.print_level>=2 {
            time_records.report_all();
        }

        ri3fn
    }

}

pub fn count_frozen_core_states(n_frozen_shell: i32, elem: &Vec<String>) -> usize {
    let mut n_low_state = 0_usize;
    let mut n_tm = 0_usize;

    //let n_frozen_shell = self.ctrl.frozen_core_postscf;
    let (n_frozen_shell_1, n_frozen_shell_2) = if n_frozen_shell > 10 {
        let n_frozen_shell_1 = n_frozen_shell%10;
        let n_frozen_shell_2 = n_frozen_shell/10;
        (n_frozen_shell_1, n_frozen_shell_2)
    } else {
        (n_frozen_shell, n_frozen_shell)
    };

    elem.iter().for_each(|sn| {
        let formated_elem = crate::geom_io::formated_element_name(&sn);
        let flag_first_row  = ELEM1ST.iter().fold(true, |acc, elem| acc || elem.eq(&formated_elem));
        let flag_second_row = if flag_first_row {
            false
        } else {
            ELEM2ND.iter().fold(true, |acc, elem| acc || elem.eq(&formated_elem))
        };
        let flag_third_row  = if flag_first_row || flag_second_row {
            false
        } else {
            ELEM3RD.iter().fold(true, |acc, elem| acc || elem.eq(&formated_elem))
        };
        let flag_fourth_row = if flag_first_row || flag_second_row || flag_third_row {
            false
        } else {
            ELEM4TH.iter().fold(true, |acc, elem| acc || elem.eq(&formated_elem))
        };
        let flag_fifth_row  = if flag_first_row || flag_second_row || flag_third_row || flag_fourth_row {
            false
        } else {
            ELEM5TH.iter().fold(true, |acc, elem| acc || elem.eq(&formated_elem))
        };
        let flag_sixth_row  = if flag_first_row || flag_second_row || flag_third_row || flag_fourth_row || flag_fifth_row {
            false
        } else {
            ELEM6TH.iter().fold(true, |acc, elem| acc || elem.eq(&formated_elem))
        };
        let flag_tm = ELEMTMS.iter().fold(true, |acc, elem| acc || elem.eq(&formated_elem));

        let n_frozen_shell_curr = if flag_tm {
            n_tm += 1;
            n_frozen_shell_2
        } else {
            n_frozen_shell_1
        };

        if flag_first_row {
            n_low_state += 0
        } else if flag_second_row {
            if n_frozen_shell_curr==0 {
                n_low_state += 0
            } else if (n_frozen_shell_curr==1) {
                n_low_state += 1
            } else {
                n_low_state += 0
            }
        } else if flag_third_row {
            if n_frozen_shell_curr==0 {
                n_low_state += 0
            } else if n_frozen_shell_curr==1 {
                n_low_state += 5
            } else if n_frozen_shell_curr==2 {
                n_low_state += 1
            } else {
                n_low_state += 0
            }
        } else if flag_fourth_row {
            if n_frozen_shell_curr==0 {
                n_low_state += 0
            } else if n_frozen_shell_curr==1 {
                n_low_state += 9
            } else if n_frozen_shell_curr==2 {
                n_low_state += 5
            } else if n_frozen_shell_curr==3 {
                n_low_state += 1
            } else {
                n_low_state += 0
            }
        } else if flag_fifth_row {
            if n_frozen_shell_curr==0 {
                n_low_state += 0
            } else if n_frozen_shell_curr==1 {
                n_low_state += 18
            } else if n_frozen_shell_curr==2 {
                // NOTE: for 4d-block elements, 3d orbitals are frozen as well for n_frozen_shell = 2
                if flag_tm {n_low_state += 14} else {n_low_state += 9}
            } else if n_frozen_shell_curr==3 {
                n_low_state += 5
            } else if n_frozen_shell_curr==4 {
                n_low_state += 1
            } else {
                n_low_state += 0
            }
        } else if flag_sixth_row {
            if n_frozen_shell_curr==0 {
                n_low_state += 0
            } else if n_frozen_shell_curr==1 {
                n_low_state += 34
            } else if n_frozen_shell_curr==2 {
                // NOTE: for 5d-block elements, 4d and 4f orbitals are frozen as well for n_frozen_shell = 2
                //       It leads to a core shell with 60 electrons
                if flag_tm {n_low_state += 30} else {n_low_state += 18}
            } else if n_frozen_shell_curr==3 {
                n_low_state += 9
            } else if n_frozen_shell_curr==4 {
                n_low_state += 5
            } else if n_frozen_shell_curr==5 {
                n_low_state += 1
            } else {
                n_low_state += 0
            }
        };
    });

    println!("First valence state for the frozen-core algorithm: {:5}", n_low_state);

    n_low_state

}
#[test]
fn test_get_slices_mut() {
    let mut test_matrix = MatrixFull::from_vec([3,3],[0,1,2,3,4,5,6,7,8].to_vec()).unwrap();
    let aa = test_matrix.iter_submatrix_mut(0..3, 0..2).map(|a| *a).collect::<Vec<i32>>();
    println!("{:?}",aa);
    let bb = test_matrix.iter_submatrix_mut(0..3, 0..2).map(|a| *a).collect::<Vec<i32>>();
    println!("{:?}",bb);
}